use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use crate::config::RepoConfig;
use crate::state::types::{Worktree, WorktreeStatus};
use crate::vcs::{VcsBackend, WorktreeSource};
use super::cli::run_jj;

pub fn is_jj_repo(path: &Path) -> bool {
    path.join(".jj").exists()
}

/// Confirms `path` is a usable jj repo (not just a stale/corrupt `.jj` dir).
/// Used once, on "Add Repo" submit — not on every refresh.
pub fn validate_jj_repo(path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();
    run_jj(&["-R", &path_str, "root"])
        .with_context(|| format!("{:?} is not a usable jj repo", path))?;
    Ok(())
}

/// Discover a jj repo's workspaces. `jj workspace list` reports names but not
/// paths, so paths are derived by gitopiary's own creation convention
/// (`<parent>/<name>`, `"default"` at the repo root) and then verified on
/// disk; workspaces that don't verify are skipped with a warning, mirroring
/// git::repo's tolerance for a `find_worktree` failure.
pub fn list_workspace_paths(config: &RepoConfig) -> Result<Vec<WorktreeSource>> {
    let repo_path_str = config.path.to_string_lossy();
    let stdout = run_jj(&["-R", &repo_path_str, "workspace", "list", "-T", "name ++ \"\\n\""])
        .with_context(|| format!("Failed to list jj workspaces for {:?}", config.path))?;

    let names = parse_workspace_names(&stdout);
    let parent = config.path.parent();

    let mut sources = vec![];
    for name in names {
        let is_main = name == "default";
        let candidate = if is_main {
            config.path.clone()
        } else {
            match parent {
                Some(p) => p.join(&name),
                None => {
                    tracing::warn!(
                        "Skipping jj workspace '{}': repo path {:?} has no parent to resolve a sibling path",
                        name, config.path
                    );
                    continue;
                }
            }
        };

        if !candidate.join(".jj").exists() {
            tracing::warn!(
                "Skipping jj workspace '{}': expected directory {:?} not found or not a jj workspace",
                name, candidate
            );
            continue;
        }

        sources.push(WorktreeSource { path: candidate, is_main, name: Some(name), backend: VcsBackend::Jj });
    }

    Ok(sources)
}

/// Parses `jj workspace list -T 'name ++ "\n"'` output, keeping only names
/// safe to use as a path component and CLI argument. jj's template engine
/// quotes names containing spaces (e.g. `"with space"`, quote characters
/// included verbatim) rather than emitting them raw, and gitopiary never
/// creates such names itself — so rather than replicate jj's quoting rules,
/// anything outside a plain identifier is skipped. This also rejects a
/// leading `-`, which `jj` CLI flags parse as an option, not a name.
pub fn parse_workspace_names(stdout: &str) -> Vec<String> {
    let mut names = vec![];
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if is_safe_workspace_name(line) {
            names.push(line.to_string());
        } else {
            tracing::warn!("Skipping jj workspace with unsupported name: {:?}", line);
        }
    }
    names
}

/// `/` is allowed (but validated per path segment) since slash-namespaced
/// names (`feature/login`) are a normal git-branch convention gitopiary's
/// own workspace-creation flow will produce, and jj emits them raw/unquoted.
/// Shared with `jj::worktree::create_workspace`, which validates
/// user-provided names before ever shelling out to `jj workspace add`.
pub(super) fn is_safe_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        })
}

/// Load a single workspace's status. Each call shells out to `jj` a handful
/// of times against `path` directly (`-R <path>`, since revsets like `@`
/// resolve relative to the target workspace).
pub fn load_workspace_info(path: PathBuf, is_main: bool, name: String) -> Result<Worktree> {
    let path_str = path.to_string_lossy().to_string();

    let (branch, bookmark) = load_branch_label(&path_str)?;
    let status = load_status(&path_str, bookmark.as_deref());

    Ok(Worktree {
        name,
        path,
        branch,
        is_main,
        status,
        pr: None,
        backend: VcsBackend::Jj,
    })
}

/// The nearest bookmark to `@` (also returned separately, since callers need
/// it to compute rebase/push status), or a short change-id if none exists —
/// the jj analog of git's "branch name, else short OID" fallback.
fn load_branch_label(path_str: &str) -> Result<(String, Option<String>)> {
    // `bookmarks.join(",")` (rather than `.map(|b| b.name()).join(",")`)
    // would render each ref's default Display form, which appends a `*`
    // whenever the local bookmark differs from a tracked remote copy — the
    // exact "unpushed commits" state this feature needs to detect, which
    // would otherwise corrupt both the displayed label and every downstream
    // revset that uses this name as a symbol.
    let bookmark_out = run_jj(&[
        "-R", path_str,
        "log", "--no-graph", "--limit", "1",
        "-r", "heads(::@ & bookmarks())",
        "-T", "bookmarks.map(|b| b.name()).join(\",\") ++ \"\\n\"",
    ])
    .unwrap_or_default();

    if let Some(bookmark) = parse_bookmark_line(&bookmark_out) {
        return Ok((bookmark.clone(), Some(bookmark)));
    }

    let change_id_out = run_jj(&[
        "-R", path_str,
        "log", "--no-graph", "--limit", "1",
        "-r", "@",
        "-T", "change_id.short(8) ++ \"\\n\"",
    ])
    .with_context(|| format!("Failed to read @ for jj workspace at {}", path_str))?;

    Ok((change_id_out.trim().to_string(), None))
}

/// Parses `jj log -T 'bookmarks.map(|b| b.name()).join(",") ++ "\n"'`
/// output. When multiple bookmarks tie at the same commit, picks the first
/// alphabetically so the choice is deterministic.
pub fn parse_bookmark_line(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = line.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if names.is_empty() {
        return None;
    }
    names.sort_unstable();
    Some(names[0].to_string())
}

fn load_status(path_str: &str, bookmark: Option<&str>) -> WorktreeStatus {
    let uncommitted_changes = run_jj(&["-R", path_str, "diff", "-r", "@", "--summary", "--no-pager"])
        .map(|out| count_nonblank_lines(&out))
        .unwrap_or(0);
    let is_dirty = uncommitted_changes > 0;
    let (ahead, behind) = get_rebase_and_push_status(path_str, bookmark);

    WorktreeStatus { uncommitted_changes, ahead, behind, is_dirty }
}

pub fn count_nonblank_lines(stdout: &str) -> u32 {
    stdout.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

/// `(ahead, behind)` for the jj analogs of "need to push" / "need to
/// rebase", scoped to `bookmark` rather than `@` directly — with no named
/// bookmark, neither question ("is *my branch* behind trunk / unpushed") has
/// an answer, so both degrade to `(0, 0)`. `bookmark` is escaped before
/// interpolation: names created via `jj bookmark create` are already
/// guaranteed safe (jj itself rejects quotes/spaces/backslashes at creation
/// time), but a bookmark can also arrive by fetching from a remote, and jj
/// does not re-validate names it imports that way — a `git` ref name can
/// contain a literal `"`, which would otherwise break out of the quoted
/// revset string.
fn get_rebase_and_push_status(path_str: &str, bookmark: Option<&str>) -> (u32, u32) {
    let Some(bookmark) = bookmark else { return (0, 0) };
    let bookmark = escape_revset_string(bookmark);

    // Commits on the bookmark not reachable from any of its remote-tracked
    // copies (unioned across every remote tracking this name) — "need to
    // push". `root()` is unioned into the left side explicitly: without it,
    // a bookmark that has never been pushed anywhere leaves
    // `remote_bookmarks(...)` empty, which would otherwise count jj's
    // synthetic root commit as part of the "unpushed" total too.
    let ahead = run_jj(&[
        "-R", path_str, "log", "--no-graph",
        "-r", &format!("(remote_bookmarks(exact:\"{bookmark}\") | root())..bookmarks(exact:\"{bookmark}\")"),
        "-T", "\"x\\n\"",
    ])
    .map(|out| count_nonblank_lines(&out))
    .unwrap_or(0);

    // Commits on `trunk()` not reachable from the bookmark — "need to
    // rebase". Any failure (including `trunk()` not resolving in a
    // brand-new repo with no bookmarks/remote) degrades to 0.
    let behind = run_jj(&["-R", path_str, "log", "--no-graph", "-r", &format!("\"{bookmark}\"..trunk()"), "-T", "\"x\\n\""])
        .map(|out| count_nonblank_lines(&out))
        .unwrap_or(0);

    (ahead, behind)
}

/// Escapes a bookmark name for embedding inside a double-quoted jj revset
/// string (e.g. `"NAME"`, `exact:"NAME"`). jj's revset language supports
/// backslash-escaped quotes inside a quoted symbol/string.
fn escape_revset_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_workspace_names_multiple() {
        assert_eq!(
            parse_workspace_names("default\nfeature-x\n"),
            vec!["default".to_string(), "feature-x".to_string()]
        );
    }

    #[test]
    fn parse_workspace_names_empty() {
        assert_eq!(parse_workspace_names(""), Vec::<String>::new());
    }

    #[test]
    fn parse_workspace_names_skips_quoted_and_unsafe_names() {
        // jj quotes names containing spaces verbatim (quote chars included)
        // rather than emitting them raw; names starting with `-` would be
        // parsed as a CLI flag by `jj` itself. Both are skipped rather than
        // mis-parsed or passed through unsafely.
        assert_eq!(
            parse_workspace_names("default\n\"with space\"\n-dashname\n"),
            vec!["default".to_string()]
        );
    }

    #[test]
    fn parse_workspace_names_allows_slash_namespaced_names() {
        // Slash-namespaced names (feature/login) are a normal git-branch
        // convention gitopiary's own workspace-creation flow will produce,
        // and jj emits them raw/unquoted — must not be dropped.
        assert_eq!(
            parse_workspace_names("default\nfeature/login\n"),
            vec!["default".to_string(), "feature/login".to_string()]
        );
    }

    #[test]
    fn parse_workspace_names_rejects_dot_segments_in_slash_names() {
        assert_eq!(
            parse_workspace_names("default\nfeature/../escape\n"),
            vec!["default".to_string()]
        );
    }

    #[test]
    fn parse_bookmark_line_present() {
        assert_eq!(parse_bookmark_line("my-feature\n"), Some("my-feature".to_string()));
    }

    #[test]
    fn parse_bookmark_line_empty() {
        assert_eq!(parse_bookmark_line("\n"), None);
        assert_eq!(parse_bookmark_line(""), None);
    }

    #[test]
    fn parse_bookmark_line_multiple_picks_first_alphabetically() {
        assert_eq!(parse_bookmark_line("zeta,alpha,mid\n"), Some("alpha".to_string()));
    }

    #[test]
    fn count_nonblank_lines_multi_and_empty() {
        assert_eq!(count_nonblank_lines("M foo\nA bar\n"), 2);
        assert_eq!(count_nonblank_lines("x\nx\nx\n"), 3);
        assert_eq!(count_nonblank_lines(""), 0);
        assert_eq!(count_nonblank_lines("\n"), 0);
    }

    /// True if `jj` is on PATH. Integration tests skip themselves rather than
    /// fail when it's absent, matching how the app degrades gracefully when
    /// jj isn't installed instead of treating it as a hard dependency.
    fn jj_available() -> bool {
        std::process::Command::new("jj")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn discovers_default_workspace_in_a_fresh_jj_repo() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();

        run_jj(&["git", "init", &repo_path_str]).unwrap();

        let config = RepoConfig { path: repo_path.clone(), name: None };
        let sources = list_workspace_paths(&config).unwrap();

        assert_eq!(sources.len(), 1);
        assert!(sources[0].is_main);
        assert_eq!(sources[0].path, repo_path);
        assert_eq!(sources[0].name.as_deref(), Some("default"));

        let wt = load_workspace_info(sources[0].path.clone(), sources[0].is_main, "default".to_string()).unwrap();
        assert!(wt.is_main);
        // Brand-new repo: no bookmarks anywhere — falls back to a change-id
        // label. Rebase/push status is scoped to a named bookmark, so with
        // none, both degrade to 0 rather than measuring anything off @.
        assert!(!wt.branch.is_empty());
        assert_eq!((wt.status.ahead, wt.status.behind), (0, 0));
    }

    #[test]
    fn uses_nearest_bookmark_as_branch_label_when_one_exists() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();
        run_jj(&["-R", &repo_path_str, "bookmark", "create", "my-feature", "-r", "@"]).unwrap();

        let wt = load_workspace_info(repo_path, true, "default".to_string()).unwrap();
        assert_eq!(wt.branch, "my-feature");
        // Never pushed anywhere: the bookmark's one real commit is
        // "unpushed" (remote_bookmarks() is empty), but root() itself must
        // NOT be counted (it's excluded explicitly). trunk() falls back to
        // root() so there's nothing to rebase onto.
        assert_eq!((wt.status.ahead, wt.status.behind), (1, 0));
    }

    #[test]
    fn discovers_secondary_workspace_by_gitopiary_naming_convention() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();

        // gitopiary's own convention: sibling directory named after the
        // workspace, verified in `list_workspace_paths` via `.jj` presence.
        let secondary_path = dir.path().join("feature-x");
        run_jj(&[
            "-R", &repo_path_str,
            "workspace", "add", "--name", "feature-x",
            &secondary_path.to_string_lossy(),
        ])
        .unwrap();

        let config = RepoConfig { path: repo_path, name: None };
        let sources = list_workspace_paths(&config).unwrap();

        assert_eq!(sources.len(), 2);
        assert!(sources.iter().any(|s| s.name.as_deref() == Some("default") && s.is_main));
        assert!(sources.iter().any(|s| s.name.as_deref() == Some("feature-x") && !s.is_main && s.path == secondary_path));
    }

    #[test]
    fn discovers_slash_namespaced_workspace() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();

        // gitopiary's <parent>/<name> convention with a slash-namespaced name
        // produces a nested directory, matching how git::worktree already
        // handles slash-namespaced branch names. Unlike `git worktree add`,
        // `jj workspace add` doesn't create intermediate directories itself.
        let secondary_path = dir.path().join("feature/login");
        std::fs::create_dir_all(secondary_path.parent().unwrap()).unwrap();
        run_jj(&[
            "-R", &repo_path_str,
            "workspace", "add", "--name", "feature/login",
            &secondary_path.to_string_lossy(),
        ])
        .unwrap();

        let config = RepoConfig { path: repo_path, name: None };
        let sources = list_workspace_paths(&config).unwrap();

        assert_eq!(sources.len(), 2);
        assert!(sources
            .iter()
            .any(|s| s.name.as_deref() == Some("feature/login") && !s.is_main && s.path == secondary_path));
    }

    /// Sets up a jj repo with a real git remote (a local bare repo), so
    /// `jj git push`/`remote_bookmarks()` behave as they would against a
    /// real origin instead of degrading to the "never pushed anywhere" case.
    /// Returns the repo's path.
    fn init_repo_with_remote(dir: &std::path::Path) -> PathBuf {
        let remote_path = dir.join("origin.git");
        std::process::Command::new("git").args(["init", "--bare", "-q"]).arg(&remote_path).output().unwrap();

        let repo_path = dir.join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();
        run_jj(&["-R", &repo_path_str, "git", "remote", "add", "origin", &remote_path.to_string_lossy()]).unwrap();

        repo_path
    }

    #[test]
    fn push_status_shows_unpushed_commits_after_local_advance() {
        if !jj_available() {
            eprintln!("skipping: git or jj not found on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo_path = init_repo_with_remote(dir.path());
        let repo_path_str = repo_path.to_string_lossy().to_string();

        run_jj(&["-R", &repo_path_str, "describe", "-m", "c1"]).unwrap();
        run_jj(&["-R", &repo_path_str, "bookmark", "create", "feature", "-r", "@"]).unwrap();
        // `--allow-new` is deprecated (as of jj 0.40) in favor of configuring
        // auto-track-bookmarks, but still works and is the simplest way to
        // publish a brand-new bookmark from a test with no existing config.
        run_jj(&["-R", &repo_path_str, "git", "push", "--bookmark", "feature", "--allow-new"]).unwrap();

        let wt = load_workspace_info(repo_path.clone(), true, "default".to_string()).unwrap();
        assert_eq!(wt.status.ahead, 0, "freshly pushed bookmark should have nothing unpushed");

        run_jj(&["-R", &repo_path_str, "new", "-m", "local change"]).unwrap();
        run_jj(&["-R", &repo_path_str, "bookmark", "set", "feature", "-r", "@"]).unwrap();

        let wt = load_workspace_info(repo_path, true, "default".to_string()).unwrap();
        assert_eq!(wt.status.ahead, 1, "one local commit not yet pushed to origin");
    }

    #[test]
    fn rebase_status_shows_commits_behind_trunk_after_divergence() {
        if !jj_available() {
            eprintln!("skipping: git or jj not found on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo_path = init_repo_with_remote(dir.path());
        let repo_path_str = repo_path.to_string_lossy().to_string();

        run_jj(&["-R", &repo_path_str, "describe", "-m", "c1"]).unwrap();
        run_jj(&["-R", &repo_path_str, "bookmark", "create", "main", "-r", "@"]).unwrap();
        run_jj(&["-R", &repo_path_str, "git", "push", "--bookmark", "main", "--allow-new"]).unwrap();

        // Branch "feature" off c1, then advance "main" separately with a
        // second pushed commit — feature is now one commit behind trunk.
        run_jj(&["-R", &repo_path_str, "new", "main", "-m", "feature work"]).unwrap();
        run_jj(&["-R", &repo_path_str, "bookmark", "create", "feature", "-r", "@"]).unwrap();
        run_jj(&["-R", &repo_path_str, "new", "main", "-m", "c2 on main"]).unwrap();
        run_jj(&["-R", &repo_path_str, "bookmark", "set", "main", "-r", "@"]).unwrap();
        run_jj(&["-R", &repo_path_str, "git", "push", "--bookmark", "main"]).unwrap();

        // Move @ onto "feature" so `heads(::@ & bookmarks())` resolves to it
        // (it's currently sitting on top of "main", the last bookmark moved).
        run_jj(&["-R", &repo_path_str, "edit", "feature"]).unwrap();

        let wt = load_workspace_info(repo_path, true, "default".to_string()).unwrap();
        assert_eq!(wt.branch, "feature");
        assert_eq!(wt.status.behind, 1, "trunk (main) has one commit feature doesn't have");
    }

    #[test]
    fn escape_revset_string_escapes_quotes_and_backslashes() {
        assert_eq!(escape_revset_string("plain"), "plain");
        assert_eq!(escape_revset_string(r#"weird"branch"#), r#"weird\"branch"#);
        assert_eq!(escape_revset_string(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn rebase_and_push_status_degrades_gracefully_for_a_bookmark_name_jj_would_never_create() {
        if !jj_available() {
            eprintln!("skipping: jj not found on PATH");
            return;
        }
        // `jj bookmark create` itself refuses a name containing `"` or a
        // space, but such a name CAN arrive by fetching from a remote (a
        // plain git ref name isn't jj-validated on import) — this exercises
        // that adversarial input directly against the function, without
        // needing to reproduce a full git-push-then-jj-fetch setup.
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo_path_str = repo_path.to_string_lossy().to_string();
        run_jj(&["git", "init", &repo_path_str]).unwrap();

        let (ahead, behind) = get_rebase_and_push_status(&repo_path_str, Some(r#"weird"branch"#));
        assert_eq!((ahead, behind), (0, 0));
    }
}
