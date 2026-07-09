pub mod handler;

use std::path::PathBuf;
use crate::github::pr::PrInfo;
use crate::state::types::Repository;

#[derive(Debug)]
pub enum AppEvent {
    Crossterm(crossterm::event::Event),
    PtyOutput { worktree_path: PathBuf },
    /// Git status for a single repo is ready — show it immediately.
    RepoLoaded(Repository),
    /// PR data for a repo arrived — patch badges onto already-visible worktrees.
    PrsFetched { repo_path: PathBuf, prs: Vec<PrInfo> },
    /// All repos in a refresh cycle have finished (git + PRs).
    RefreshDone,
    RefreshError(String),
    WorktreeCreated { repo_path: PathBuf, worktree_path: PathBuf },
    WorktreeCreateError(String),
    WorktreeDeleted { repo_path: PathBuf, worktree_path: PathBuf },
    WorktreeDeleteError(String),
    RepoAdded(PathBuf),
    RepoAddError(String),
    /// Periodic 1-second heartbeat used to update idle indicators.
    Tick,
    /// Periodic refresh-interval heartbeat. Routed through
    /// `App::trigger_refresh` (rather than starting a refresh directly)
    /// so it shares that function's `is_refreshing` guard — otherwise this
    /// timer firing while an explicitly-triggered refresh is still running
    /// would start a second, fully concurrent refresh cycle over the same
    /// repos. For jj repos specifically, two independent processes both
    /// querying the same repo concurrently can hit jj's repo-level working-
    /// copy lock, intermittently and non-deterministically dropping a
    /// workspace from the result (a real bug this fixes).
    RefreshTick,
    Quit,
}
