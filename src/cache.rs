use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use crate::config::RepoConfig;
use crate::state::types::{PrState, PullRequest, Repository, Worktree, WorktreeStatus};
use crate::vcs::VcsBackend;

// ---------------------------------------------------------------------------
// Serialisable mirror types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Cache {
    pub repos: Vec<CachedRepo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedRepo {
    pub path: PathBuf,
    /// "git" | "jj". Optional so cache files written before jj support
    /// existed still load cleanly.
    #[serde(default)]
    pub backend: Option<String>,
    pub worktrees: Vec<CachedWorktree>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedWorktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub is_main: bool,
    pub uncommitted_changes: u32,
    pub ahead: u32,
    pub behind: u32,
    pub is_dirty: bool,
    pub pr: Option<CachedPr>,
    /// "git" | "jj", per-entry (a colocated repo can mix both). Optional so
    /// cache files written before jj support existed still load cleanly.
    #[serde(default)]
    pub backend: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedPr {
    pub number: u64,
    pub title: String,
    /// "open" | "closed" | "merged"
    pub state: String,
    pub is_draft: bool,
    pub url: String,
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

pub fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("gitopiary")
        .join("worktrees.json")
}

/// Load the cache from disk. Returns an empty cache on any error so startup
/// is never blocked.
pub fn load() -> Cache {
    let path = cache_path();
    let Ok(bytes) = std::fs::read(&path) else { return Cache::default() };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Persist fresh repo data to disk. Errors are logged and ignored — the
/// cache is best-effort and must never break the app.
pub fn save(repos: &[Repository]) {
    let cache = Cache {
        repos: repos.iter().map(repo_to_cached).collect(),
    };
    let Ok(json) = serde_json::to_vec_pretty(&cache) else {
        tracing::warn!("Failed to serialise cache");
        return;
    };
    let path = cache_path();
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("Failed to create cache dir: {}", e);
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, json) {
        tracing::warn!("Failed to write cache: {}", e);
    }
}

// ---------------------------------------------------------------------------
// Conversion: Cache → state types
// ---------------------------------------------------------------------------

/// Build a `Repository` pre-populated with cached worktrees for `config`.
/// If there is no cached entry for this path, returns an empty repository.
pub fn hydrate_repo(config: RepoConfig, cache: &Cache) -> Repository {
    let mut repo = Repository::new(config.clone());

    if let Some(cached) = cache.repos.iter().find(|r| r.path == config.path) {
        repo.worktrees = cached.worktrees.iter().map(cached_to_worktree).collect();
        if let Some(backend) = &cached.backend {
            repo.backend = match backend.as_str() {
                "jj" => VcsBackend::Jj,
                _ => VcsBackend::Git,
            };
        }
    }

    repo
}

fn cached_to_worktree(w: &CachedWorktree) -> Worktree {
    Worktree {
        name: w.name.clone(),
        path: w.path.clone(),
        branch: w.branch.clone(),
        is_main: w.is_main,
        status: WorktreeStatus {
            uncommitted_changes: w.uncommitted_changes,
            ahead: w.ahead,
            behind: w.behind,
            is_dirty: w.is_dirty,
        },
        pr: w.pr.as_ref().map(cached_to_pr),
        backend: match w.backend.as_deref() {
            Some("jj") => VcsBackend::Jj,
            _ => VcsBackend::Git,
        },
    }
}

fn cached_to_pr(p: &CachedPr) -> PullRequest {
    PullRequest {
        number: p.number,
        title: p.title.clone(),
        state: match p.state.as_str() {
            "merged" => PrState::Merged,
            "closed" => PrState::Closed,
            _ => PrState::Open,
        },
        is_draft: p.is_draft,
        url: p.url.clone(),
    }
}

// ---------------------------------------------------------------------------
// Conversion: state types → Cache
// ---------------------------------------------------------------------------

fn repo_to_cached(repo: &Repository) -> CachedRepo {
    CachedRepo {
        path: repo.config.path.clone(),
        backend: Some(match repo.backend {
            VcsBackend::Git => "git",
            VcsBackend::Jj => "jj",
        }.to_string()),
        worktrees: repo.worktrees.iter().map(worktree_to_cached).collect(),
    }
}

fn worktree_to_cached(w: &Worktree) -> CachedWorktree {
    CachedWorktree {
        name: w.name.clone(),
        path: w.path.clone(),
        branch: w.branch.clone(),
        is_main: w.is_main,
        uncommitted_changes: w.status.uncommitted_changes,
        ahead: w.status.ahead,
        behind: w.status.behind,
        is_dirty: w.status.is_dirty,
        pr: w.pr.as_ref().map(pr_to_cached),
        backend: Some(match w.backend {
            VcsBackend::Git => "git",
            VcsBackend::Jj => "jj",
        }.to_string()),
    }
}

fn pr_to_cached(p: &PullRequest) -> CachedPr {
    CachedPr {
        number: p.number,
        title: p.title.clone(),
        state: match p.state {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
        }
        .to_string(),
        is_draft: p.is_draft,
        url: p.url.clone(),
    }
}
