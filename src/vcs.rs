use std::path::{Path, PathBuf};
use anyhow::Result;
use crate::config::RepoConfig;
use crate::state::types::Worktree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsBackend {
    Git,
    Jj,
}

/// A repo is jj-backed if it has a `.jj` directory, colocated with git or not.
pub fn detect_backend(repo_path: &Path) -> VcsBackend {
    if repo_path.join(".jj").exists() {
        VcsBackend::Jj
    } else {
        VcsBackend::Git
    }
}

/// Validates a path is usable as either backend, preferring jj to match
/// `detect_backend`'s priority (a colocated repo is treated as jj-native).
/// Used once, on "Add Repo" submit.
pub fn validate_repo_path(path: &Path) -> Result<VcsBackend, String> {
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }

    if crate::jj::repo::is_jj_repo(path) {
        return crate::jj::repo::validate_jj_repo(path)
            .map(|_| VcsBackend::Jj)
            .map_err(|e| e.to_string());
    }

    git2::Repository::open(path)
        .map(|_| VcsBackend::Git)
        .map_err(|e| format!("Not a git or jj repository: {}", e))
}

/// One worktree/workspace directory discovered under a repo. `backend` is
/// per-entry rather than repo-wide: a colocated repo can have plain git
/// worktrees (created via `git worktree add`, unknown to jj) and jj
/// workspaces (created via `jj workspace add`, unknown to git) at the same
/// time, so each discovered entry remembers which backend actually owns it.
/// `name` is only meaningful for jj (the workspace name); git worktrees
/// derive their name from the path's file name instead. `repo_path` is the
/// repo root (same as `path` for the main entry) — jj status queries other
/// than the dirty-file count run against it rather than `path` directly, so
/// they work even if a *secondary* jj workspace's own working copy is
/// stale. `commit_id` (jj only) is that workspace's current commit, queried
/// once from the repo-level `jj workspace list` (which doesn't require
/// entering the workspace itself, so it's available even when stale).
pub struct WorktreeSource {
    pub path: PathBuf,
    pub is_main: bool,
    pub name: Option<String>,
    pub backend: VcsBackend,
    pub repo_path: PathBuf,
    pub commit_id: Option<String>,
}

/// Discovers worktrees/workspaces for a repo by UNIONING both backends
/// rather than picking one exclusively: git worktree enumeration runs
/// whenever `.git` exists, jj workspace enumeration runs whenever `.jj`
/// exists, and results are merged by path. A path found by both (the main
/// checkout directory, which is simultaneously git's "main" worktree and
/// jj's "default" workspace when colocated) is reported once, tagged jj —
/// jj's status is the richer of the two for that shared entry. This is NOT
/// gated on `detect_backend`: a colocated repo with git worktrees the jj
/// workspace list has never heard of (or vice versa) must still show all of
/// them, not silently drop the ones the "preferred" backend doesn't know
/// about.
pub fn list_worktree_paths(config: &RepoConfig) -> Result<Vec<WorktreeSource>> {
    let mut sources: Vec<WorktreeSource> = vec![];

    match crate::git::repo::list_worktree_paths(config) {
        Ok(git_paths) => sources.extend(git_paths.into_iter().map(|(path, is_main)| WorktreeSource {
            path,
            is_main,
            name: None,
            backend: VcsBackend::Git,
            repo_path: config.path.clone(),
            commit_id: None,
        })),
        // A plain directory with no `.git` at all hits this too (expected,
        // silent) — but so would a real git2 failure on an actual git repo,
        // which must not vanish without a trace the way this whole fix is
        // about preventing.
        Err(e) => tracing::warn!("Failed to list git worktrees for {:?}: {}", config.path, e),
    }

    if crate::jj::repo::is_jj_repo(&config.path) {
        match crate::jj::repo::list_workspace_paths(config) {
            Ok(jj_sources) => {
                for jj_source in jj_sources {
                    if let Some(existing) = sources.iter_mut().find(|s| paths_match(&s.path, &jj_source.path)) {
                        existing.backend = VcsBackend::Jj;
                        existing.name = jj_source.name;
                        existing.commit_id = jj_source.commit_id;
                    } else {
                        sources.push(jj_source);
                    }
                }
            }
            Err(e) => tracing::warn!("Failed to list jj workspaces for {:?}: {}", config.path, e),
        }
    }

    if sources.is_empty() {
        anyhow::bail!("Not a git or jj repository: {:?}", config.path);
    }

    Ok(sources)
}

/// Compares paths the way the OS would, not byte-for-byte: git2 canonicalizes
/// linked-worktree paths (e.g. resolving macOS's `/var` -> `/private/var`
/// symlink) but jj-discovered paths are a direct pass-through of the
/// configured repo path, so a naive `==` can fail to recognize the same
/// directory reached two different ways. Falls back to the raw comparison
/// when canonicalization fails (e.g. a path that doesn't exist).
fn paths_match(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// True if any source in this repo is jj-backed. jj commands snapshot the
/// working copy and take a repo-level lock, so a repo with any jj source
/// must load all of its sources sequentially to avoid lock-contention
/// errors between them — a repo with only git sources can stay fully
/// parallel, since each git2 call is independent.
pub fn needs_sequential_loading(sources: &[WorktreeSource]) -> bool {
    sources.iter().any(|s| s.backend == VcsBackend::Jj)
}

pub fn load_worktree_info(source: WorktreeSource) -> Result<Worktree> {
    match source.backend {
        VcsBackend::Git => crate::git::repo::load_worktree_info(source.path, source.is_main),
        // `commit_id` is always `Some` here: every `Jj`-tagged `WorktreeSource`
        // is either constructed by `jj::repo::list_workspace_paths` (which
        // always sets it) or upgraded from a `Git` entry during the merge in
        // `list_worktree_paths`, which copies it over at the same time.
        VcsBackend::Jj => crate::jj::repo::load_workspace_info(
            &source.repo_path,
            source.path,
            source.is_main,
            source.name.unwrap_or_default(),
            source.commit_id.unwrap_or_default(),
        ),
    }
}

pub async fn create_workspace(backend: VcsBackend, repo_path: &PathBuf, name: &str) -> Result<PathBuf> {
    match backend {
        VcsBackend::Git => crate::git::worktree::create_worktree(repo_path, name).await,
        VcsBackend::Jj => crate::jj::worktree::create_workspace(repo_path, name).await,
    }
}

pub async fn remove_workspace(
    backend: VcsBackend,
    repo_path: &PathBuf,
    workspace_path: &PathBuf,
    name: &str,
) -> Result<()> {
    match backend {
        VcsBackend::Git => crate::git::worktree::remove_worktree(repo_path, workspace_path).await,
        VcsBackend::Jj => crate::jj::worktree::remove_workspace(repo_path, workspace_path, name).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_jj_when_dot_jj_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".jj")).unwrap();
        assert_eq!(detect_backend(dir.path()), VcsBackend::Jj);
    }

    #[test]
    fn detects_git_when_no_dot_jj() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(detect_backend(dir.path()), VcsBackend::Git);
    }

    #[test]
    fn detects_git_as_fallback_when_neither_present() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_backend(dir.path()), VcsBackend::Git);
    }

    fn tools_available() -> bool {
        std::process::Command::new("git").arg("--version").output().is_ok()
            && std::process::Command::new("jj").arg("--version").output().is_ok()
    }

    fn source(path: &str, backend: VcsBackend) -> WorktreeSource {
        WorktreeSource {
            path: PathBuf::from(path),
            is_main: false,
            name: None,
            backend,
            repo_path: PathBuf::from(path),
            commit_id: None,
        }
    }

    #[test]
    fn needs_sequential_loading_true_when_any_source_is_jj() {
        let sources = vec![source("/a", VcsBackend::Git), source("/b", VcsBackend::Jj)];
        assert!(needs_sequential_loading(&sources));
    }

    #[test]
    fn needs_sequential_loading_false_when_all_git() {
        let sources = vec![source("/a", VcsBackend::Git), source("/b", VcsBackend::Git)];
        assert!(!needs_sequential_loading(&sources));
    }

    #[test]
    fn needs_sequential_loading_false_when_empty() {
        assert!(!needs_sequential_loading(&[]));
    }

    #[test]
    fn validate_repo_path_rejects_nonexistent_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(validate_repo_path(&missing).is_err());
    }

    #[test]
    fn validate_repo_path_rejects_plain_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_repo_path(dir.path()).is_err());
    }

    #[test]
    fn validate_repo_path_accepts_plain_git_repo() {
        if !tools_available() {
            eprintln!("skipping: git not found on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        std::process::Command::new("git").arg("init").arg("-q").arg(&repo_path).output().unwrap();

        assert_eq!(validate_repo_path(&repo_path), Ok(VcsBackend::Git));
    }

    #[test]
    fn validate_repo_path_accepts_non_colocated_jj_repo() {
        if !tools_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        // `--colocate` is jj's default as of 0.40 (`jj git init` alone still
        // creates a top-level `.git`); a truly non-colocated repo (no `.git`
        // at all, git backend hidden under `.jj/repo/store`) needs the
        // `git.colocate=false` config override. These must validate too.
        crate::jj::cli::run_jj(&["--config", "git.colocate=false", "git", "init", &repo_path.to_string_lossy()]).unwrap();
        assert!(!repo_path.join(".git").exists());

        assert_eq!(validate_repo_path(&repo_path), Ok(VcsBackend::Jj));
    }

    #[test]
    fn validate_repo_path_prefers_jj_for_colocated_repo() {
        if !tools_available() {
            eprintln!("skipping: git or jj not found on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        std::process::Command::new("git").arg("init").arg("-q").arg(&repo_path).output().unwrap();
        crate::jj::cli::run_jj(&["git", "init", "--colocate", &repo_path.to_string_lossy()]).unwrap();

        assert_eq!(validate_repo_path(&repo_path), Ok(VcsBackend::Jj));
    }

    /// Regression test: a colocated repo (`.git` + `.jj`) with a plain
    /// `git worktree add`-created worktree must still show it, even though
    /// `jj workspace list` has never heard of it (jj and git worktrees are
    /// two independent registries). A real user hit this: `detect_backend`
    /// treating `.jj` presence as "discover via jj only" silently dropped
    /// every git-worktree-created directory from repos they'd colocated.
    #[test]
    fn colocated_repo_keeps_git_worktrees_jj_does_not_know_about() {
        if !tools_available() {
            eprintln!("skipping: git or jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();

        std::process::Command::new("git").arg("init").arg("-q").arg(&repo_path).output().unwrap();
        std::process::Command::new("git")
            .args(["-c", "user.email=t@t.com", "-c", "user.name=t", "commit", "--allow-empty", "-q", "-m", "init"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        crate::jj::cli::run_jj(&["git", "init", "--colocate", &repo_path_str]).unwrap();

        let worktree_path = dir.path().join("feature-y");
        std::process::Command::new("git")
            .args(["worktree", "add", "-b", "feature-y"])
            .arg(&worktree_path)
            .current_dir(&repo_path)
            .output()
            .unwrap();

        let config = RepoConfig { path: repo_path.clone(), name: None };
        let sources = list_worktree_paths(&config).unwrap();

        // git2 canonicalizes linked-worktree paths (resolving macOS's /var
        // -> /private/var symlink) but the jj-discovered main path is a
        // direct pass-through of `config.path`, so only the former needs
        // canonicalizing before comparison.
        let worktree_path = worktree_path.canonicalize().unwrap();

        assert_eq!(sources.len(), 2, "expected main + the git-created worktree, got {:?}", sources.iter().map(|s| &s.path).collect::<Vec<_>>());
        assert!(sources.iter().any(|s| s.path == worktree_path && s.backend == VcsBackend::Git));
        // The shared main/default path is reported once, tagged jj.
        assert!(sources.iter().any(|s| s.path == repo_path && s.is_main && s.backend == VcsBackend::Jj));
    }
}
