use std::path::PathBuf;
use git2::{BranchType, Repository as GitRepo, StatusOptions};
use crate::config::RepoConfig;
use crate::state::types::{Repository, Worktree, WorktreeStatus};
use crate::vcs::VcsBackend;
use anyhow::{Context, Result};

/// Fast first pass: open the repo just long enough to enumerate worktree paths.
/// Returns (path, is_main) pairs. Does not compute status.
pub fn list_worktree_paths(config: &RepoConfig) -> Result<Vec<(PathBuf, bool)>> {
    let git_repo = GitRepo::open(&config.path)
        .with_context(|| format!("Failed to open git repo at {:?}", config.path))?;

    let mut paths = vec![(config.path.clone(), true)];

    let linked = git_repo
        .worktrees()
        .with_context(|| "Failed to list worktrees")?;

    for name in linked.iter() {
        let name = match name {
            Some(n) => n,
            None => continue,
        };
        let wt_obj = match git_repo.find_worktree(name) {
            Ok(w) => w,
            Err(_) => continue,
        };
        paths.push((wt_obj.path().to_path_buf(), false));
    }

    Ok(paths)
}

/// Load a single worktree's branch name and git status. Each call opens its
/// own independent git2::Repository so calls can run on separate threads.
pub fn load_worktree_info(path: PathBuf, is_main: bool) -> Result<Worktree> {
    let git_repo = GitRepo::open(&path)
        .with_context(|| format!("Failed to open worktree at {:?}", path))?;

    let branch = get_branch_name(&git_repo).unwrap_or_else(|| "HEAD".to_string());

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| branch.clone());

    let status = get_worktree_status(&git_repo)?;

    Ok(Worktree {
        name,
        path,
        branch,
        is_main,
        status,
        pr: None,
        backend: VcsBackend::Git,
    })
}

fn get_branch_name(repo: &GitRepo) -> Option<String> {
    let head = repo.head().ok()?;
    if head.is_branch() {
        head.shorthand().map(|s| s.to_string())
    } else {
        head.target()
            .map(|oid| oid.to_string()[..8].to_string())
    }
}

fn get_worktree_status(repo: &GitRepo) -> Result<WorktreeStatus> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true);
    opts.exclude_submodules(true);

    let statuses = repo
        .statuses(Some(&mut opts))
        .with_context(|| "Failed to get repo statuses")?;

    let uncommitted_changes = statuses
        .iter()
        .filter(|s| {
            s.status().intersects(
                git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_DELETED
                    | git2::Status::INDEX_RENAMED
                    | git2::Status::INDEX_TYPECHANGE
                    | git2::Status::WT_NEW
                    | git2::Status::WT_MODIFIED
                    | git2::Status::WT_DELETED
                    | git2::Status::WT_RENAMED
                    | git2::Status::WT_TYPECHANGE,
            )
        })
        .count() as u32;

    let is_dirty = uncommitted_changes > 0;
    let (ahead, behind) = get_ahead_behind(repo).unwrap_or((0, 0));

    Ok(WorktreeStatus {
        uncommitted_changes,
        ahead,
        behind,
        is_dirty,
    })
}

fn get_ahead_behind(repo: &GitRepo) -> Option<(u32, u32)> {
    let head = repo.head().ok()?;
    let branch_name = head.shorthand()?;

    let local_branch = repo.find_branch(branch_name, BranchType::Local).ok()?;
    let upstream = local_branch.upstream().ok()?;
    let local_oid = head.target()?;
    let upstream_oid = upstream.get().target()?;

    let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid).ok()?;
    Some((ahead as u32, behind as u32))
}
