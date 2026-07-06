use std::path::{Path, PathBuf};
use anyhow::{bail, Context, Result};
use super::cli::run_jj_async;
use super::repo::is_safe_workspace_name;

/// Creates a jj workspace in a sibling directory: `<parent>/<name>`, mirroring
/// `git::worktree::create_worktree`'s convention. Unlike `git worktree add`,
/// `jj workspace add` does not create intermediate directories itself, so a
/// slash-namespaced name (`feature/login`) needs its parent directory made
/// first.
pub async fn create_workspace(repo_path: &Path, name: &str) -> Result<PathBuf> {
    if !is_safe_workspace_name(name) {
        bail!("Invalid workspace name: {:?}", name);
    }

    let parent = repo_path
        .parent()
        .with_context(|| "Repo path has no parent")?;
    let workspace_path = parent.join(name);

    if let Some(wt_parent) = workspace_path.parent() {
        tokio::fs::create_dir_all(wt_parent)
            .await
            .with_context(|| format!("Failed to create parent directory for {:?}", workspace_path))?;
    }

    let repo_path_str = repo_path.to_string_lossy();
    let workspace_path_str = workspace_path.to_string_lossy();

    run_jj_async(&[
        "-R", &repo_path_str,
        "workspace", "add", "--name", name,
        &workspace_path_str,
    ])
    .await?;

    Ok(workspace_path)
}

/// Removes a jj workspace. Unlike `git worktree remove`, `jj workspace
/// forget` only deregisters the workspace — it does not delete the
/// directory — so this is a two-step operation. If `forget` fails, the
/// directory is left untouched. If `forget` succeeds but the directory
/// delete fails, the workspace is still gone from jj's perspective, so this
/// only logs a warning rather than reporting failure.
pub async fn remove_workspace(repo_path: &Path, workspace_path: &Path, name: &str) -> Result<()> {
    let repo_path_str = repo_path.to_string_lossy();
    run_jj_async(&["-R", &repo_path_str, "workspace", "forget", name])
        .await
        .with_context(|| format!("Failed to forget jj workspace '{}'", name))?;

    if let Err(e) = tokio::fs::remove_dir_all(workspace_path).await {
        tracing::warn!(
            "jj workspace '{}' forgotten but failed to delete directory {:?}: {}",
            name, workspace_path, e
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::cli::run_jj;

    fn jj_available() -> bool {
        std::process::Command::new("jj")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn create_workspace_rejects_unsafe_name_without_shelling_out() {
        // No jj_available() guard: this must fail on validation alone,
        // before ever spawning jj, so it should behave the same with or
        // without jj installed.
        let repo_path = PathBuf::from("/nonexistent/repo");
        let result = create_workspace(&repo_path, "-dangerous").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_then_remove_workspace_round_trip() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();

        let workspace_path = create_workspace(&repo_path, "feature-x").await.unwrap();
        assert_eq!(workspace_path, dir.path().join("feature-x"));
        assert!(workspace_path.join(".jj").exists());

        let list_before = run_jj(&["-R", &repo_path_str, "workspace", "list", "-T", "name ++ \"\\n\""]).unwrap();
        assert!(list_before.contains("feature-x"));

        remove_workspace(&repo_path, &workspace_path, "feature-x").await.unwrap();

        assert!(!workspace_path.exists());
        let list_after = run_jj(&["-R", &repo_path_str, "workspace", "list", "-T", "name ++ \"\\n\""]).unwrap();
        assert!(!list_after.contains("feature-x"));
    }

    #[tokio::test]
    async fn create_workspace_with_slash_name_creates_nested_directory() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();

        // Unlike `git worktree add`, `jj workspace add` doesn't create
        // intermediate directories itself — this is the regression case for
        // that behavior.
        let workspace_path = create_workspace(&repo_path, "feature/login").await.unwrap();
        assert_eq!(workspace_path, dir.path().join("feature").join("login"));
        assert!(workspace_path.join(".jj").exists());
    }

    #[tokio::test]
    async fn remove_workspace_leaves_directory_when_forget_fails() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        // `repo_path` isn't a jj repo at all (no `.jj`), so `-R repo_path`
        // itself fails and `workspace forget` never gets a chance to run —
        // unlike forgetting an unregistered name under a *real* repo, which
        // jj treats as a no-op success, not an error.
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("not-a-repo");
        std::fs::create_dir(&repo_path).unwrap();

        let target_path = dir.path().join("some-workspace");
        std::fs::create_dir(&target_path).unwrap();

        let result = remove_workspace(&repo_path, &target_path, "some-workspace").await;
        assert!(result.is_err());
        assert!(target_path.exists());
    }

    #[tokio::test]
    async fn remove_workspace_succeeds_when_forget_ok_but_directory_delete_fails() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        // Unlike the "not a jj repo" case above, `jj workspace forget` on a
        // name that IS registered succeeds even if the path passed here
        // doesn't match (or doesn't exist) — jj only tracks the name, not
        // this function's `workspace_path` argument. That's the scenario
        // where directory removal can fail independently of `forget`, and
        // the workspace is genuinely gone from jj's perspective by then, so
        // this must still report overall success.
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();

        let workspace_path = create_workspace(&repo_path, "feature-x").await.unwrap();

        let bogus_path = dir.path().join("does-not-exist");
        let result = remove_workspace(&repo_path, &bogus_path, "feature-x").await;
        assert!(result.is_ok());

        let list_after = run_jj(&["-R", &repo_path_str, "workspace", "list", "-T", "name ++ \"\\n\""]).unwrap();
        assert!(!list_after.contains("feature-x"));
        // The real directory was never touched, since we passed a bogus one.
        assert!(workspace_path.exists());
    }
}
