use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;
use crate::config::{Config, RepoConfig};
use crate::events::AppEvent;
use crate::github::pr::fetch_prs;
use crate::state::types::Repository;
use crate::vcs;

pub async fn run_refresh(config: Config, tx: UnboundedSender<AppEvent>) {
    let mut interval = tokio::time::interval(
        std::time::Duration::from_secs(config.refresh_interval_secs),
    );
    loop {
        interval.tick().await;
        do_refresh(&config, &tx).await;
    }
}

pub async fn refresh_once(config: &Config, tx: &UnboundedSender<AppEvent>) {
    do_refresh(config, tx).await;
}

/// Loads all repos in parallel. For each repo:
///   1. List worktree paths (fast, blocking)
///   2. Load each worktree's git status in parallel (blocking, one thread per worktree)
///   3. Send RepoLoaded — UI shows git data immediately
///   4. Fetch PRs (async, overlaps with other repos still loading)
///   5. Send PrsFetched — UI patches in PR badges
/// Sends RefreshDone when everything is complete.
async fn do_refresh(config: &Config, tx: &UnboundedSender<AppEvent>) {
    let mut set: JoinSet<()> = JoinSet::new();

    for repo_config in &config.repos {
        let repo_cfg = repo_config.clone();
        let tx = tx.clone();
        set.spawn(load_repo_streaming(repo_cfg, tx));
    }

    while set.join_next().await.is_some() {}

    tx.send(AppEvent::RefreshDone).ok();
}

async fn load_repo_streaming(config: RepoConfig, tx: UnboundedSender<AppEvent>) {
    // Repo-level backend: only used as the default for NEW creation (the
    // New Worktree dialog). Discovery itself unions both backends below, so
    // existing worktrees/workspaces are never gated on this value.
    let backend = vcs::detect_backend(&config.path);

    // Phase 1: enumerate worktree paths — open the repo once, quickly.
    let config_for_list = config.clone();
    let sources = match tokio::task::spawn_blocking(move || {
        vcs::list_worktree_paths(&config_for_list)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::warn!("Failed to list worktrees for {:?}: {}", config.path, e);
            tx.send(AppEvent::RefreshError(e.to_string())).ok();
            return;
        }
        Err(e) => {
            tracing::warn!("Join error for {:?}: {}", config.path, e);
            return;
        }
    };

    // Phase 2: load each worktree's status.
    let mut worktrees = if vcs::needs_sequential_loading(&sources) {
        let mut worktrees = vec![];
        for source in sources {
            let result = tokio::task::spawn_blocking(move || vcs::load_worktree_info(source)).await;
            match result {
                Ok(Ok(wt)) => worktrees.push(wt),
                Ok(Err(e)) => tracing::warn!("Failed to load worktree status: {}", e),
                Err(e) => tracing::warn!("Worktree thread error: {}", e),
            }
        }
        worktrees
    } else {
        let mut wt_set: JoinSet<anyhow::Result<crate::state::types::Worktree>> = JoinSet::new();
        for source in sources {
            wt_set.spawn_blocking(move || vcs::load_worktree_info(source));
        }
        let mut worktrees = vec![];
        while let Some(result) = wt_set.join_next().await {
            match result {
                Ok(Ok(wt)) => worktrees.push(wt),
                Ok(Err(e)) => tracing::warn!("Failed to load worktree status: {}", e),
                Err(e) => tracing::warn!("Worktree thread error: {}", e),
            }
        }
        worktrees
    };

    // Keep main worktree first, then sort the rest alphabetically.
    worktrees.sort_by(|a, b| b.is_main.cmp(&a.is_main).then(a.name.cmp(&b.name)));

    let mut repo = Repository::new(config.clone());
    repo.backend = backend;
    repo.worktrees = worktrees;

    // Phase 3: send git status — the UI renders this immediately.
    tx.send(AppEvent::RepoLoaded(repo)).ok();

    // Phase 4: fetch PRs. This runs concurrently with other repos still in
    // phases 1-3, so it doesn't block the display of other repos.
    let prs = fetch_prs(&config.path).await.unwrap_or_default();
    tx.send(AppEvent::PrsFetched {
        repo_path: config.path,
        prs,
    })
    .ok();
}

pub async fn create_worktree(
    repo_path: PathBuf,
    name: String,
    tx: UnboundedSender<AppEvent>,
) {
    let backend = vcs::detect_backend(&repo_path);
    let result = vcs::create_workspace(backend, &repo_path, &name).await;
    match result {
        Ok(worktree_path) => {
            tx.send(AppEvent::WorktreeCreated { repo_path, worktree_path }).ok();
        }
        Err(e) => {
            tx.send(AppEvent::WorktreeCreateError(e.to_string())).ok();
        }
    }
}
