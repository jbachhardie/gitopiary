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

/// One worktree/workspace directory discovered under a repo, backend-agnostic.
/// `name` is only meaningful for jj (the workspace name); git worktrees derive
/// their name from the path's file name instead.
pub struct WorktreeSource {
    pub path: PathBuf,
    pub is_main: bool,
    pub name: Option<String>,
}

pub fn list_worktree_paths(backend: VcsBackend, config: &RepoConfig) -> Result<Vec<WorktreeSource>> {
    match backend {
        VcsBackend::Git => Ok(crate::git::repo::list_worktree_paths(config)?
            .into_iter()
            .map(|(path, is_main)| WorktreeSource { path, is_main, name: None })
            .collect()),
        VcsBackend::Jj => crate::jj::repo::list_workspace_paths(config),
    }
}

pub fn load_worktree_info(backend: VcsBackend, source: WorktreeSource) -> Result<Worktree> {
    match backend {
        VcsBackend::Git => crate::git::repo::load_worktree_info(source.path, source.is_main),
        VcsBackend::Jj => crate::jj::repo::load_workspace_info(
            source.path,
            source.is_main,
            source.name.unwrap_or_default(),
        ),
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
}
