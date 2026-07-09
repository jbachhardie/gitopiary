use std::io::{self, Stdout};
use anyhow::Result;
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, EventStream},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use crate::config::{Config, RepoConfig, save_config};
use crate::events::{handler::handle_event, AppEvent};
use crate::pty::manager::PtyManager;
use crate::state::{refresh::run_refresh, types::AppState};
use crate::ui::draw;

pub struct App {
    pub state: AppState,
    pub pty_manager: PtyManager,
    pub terminal_size: (u16, u16),
    pub config: Config,
    /// Exact inner size of the terminal panel from the last rendered frame.
    /// Use this (when non-zero) for new PTY sessions instead of the approximation
    /// from compute_terminal_pty_size, so TUI programs see the right size immediately.
    pub last_synced_inner: (u16, u16),
}

impl App {
    pub fn new(state: AppState, config: Config) -> Self {
        let shell = config.shell.clone();
        Self {
            state,
            pty_manager: PtyManager::new(shell),
            terminal_size: (80, 24),
            config,
            last_synced_inner: (0, 0),
        }
    }

    fn clamp_selection(&mut self) {
        if self.state.repos.is_empty() {
            self.state.selected_repo_idx = 0;
            self.state.selected_worktree_idx = 0;
            return;
        }
        self.state.selected_repo_idx =
            self.state.selected_repo_idx.min(self.state.repos.len() - 1);
        let wt_count = self.state.repos[self.state.selected_repo_idx]
            .worktrees
            .len();
        if wt_count > 0 {
            self.state.selected_worktree_idx =
                self.state.selected_worktree_idx.min(wt_count - 1);
        } else {
            self.state.selected_worktree_idx = 0;
        }
    }

    pub fn trigger_refresh(&mut self, tx: UnboundedSender<AppEvent>) {
        if self.state.is_refreshing {
            return;
        }
        self.state.is_refreshing = true;
        let config = self.config.clone();
        tokio::spawn(async move {
            crate::state::refresh::refresh_once(&config, &tx).await;
        });
    }

    pub async fn run(mut self) -> Result<()> {
        let (tx, rx) = mpsc::unbounded_channel::<AppEvent>();

        // Initial terminal size
        let size = crossterm::terminal::size()?;
        self.terminal_size = size;

        // Setup terminal
        let mut terminal = setup_terminal()?;

        // Spawn crossterm event reader
        let tx_crossterm = tx.clone();
        tokio::spawn(async move {
            let mut events = EventStream::new();
            while let Some(event) = events.next().await {
                match event {
                    Ok(e) => {
                        if tx_crossterm.send(AppEvent::Crossterm(e)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Crossterm event error: {}", e);
                        break;
                    }
                }
            }
        });

        // Spawn 1-second tick for idle indicators in the worktree panel.
        let tx_tick = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                if tx_tick.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });

        // Spawn background refresh
        let tx_refresh = tx.clone();
        let config = self.config.clone();
        tokio::spawn(run_refresh(config, tx_refresh));

        // Initial refresh
        self.trigger_refresh(tx.clone());

        let result = self.event_loop(&mut terminal, rx, tx).await;

        restore_terminal(&mut terminal)?;
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        mut rx: UnboundedReceiver<AppEvent>,
        tx: UnboundedSender<AppEvent>,
    ) -> Result<()> {
        // Initial draw — capture exact inner size and sync PTYs immediately.
        let inner = std::cell::Cell::new((0u16, 0u16));
        terminal.draw(|f| { inner.set(draw(f, self)); })?;
        self.sync_pty_sizes(inner.get());

        while let Some(event) = rx.recv().await {
            let needs_redraw = self.process_event(event, &tx);
            if self.state.should_quit {
                break;
            }
            if needs_redraw {
                terminal.draw(|f| { inner.set(draw(f, self)); })?;
                self.sync_pty_sizes(inner.get());
            }
        }

        Ok(())
    }

    /// Resize all PTY sessions to match the actual rendered inner area,
    /// but only when the size has actually changed so we don't spam SIGWINCH.
    fn sync_pty_sizes(&mut self, inner: (u16, u16)) {
        let (cols, rows) = inner;
        if cols == 0 || rows == 0 || inner == self.last_synced_inner {
            return;
        }
        self.last_synced_inner = inner;
        self.pty_manager.resize_all(rows, cols);
    }

    fn process_event(&mut self, event: AppEvent, tx: &UnboundedSender<AppEvent>) -> bool {
        match event {
            AppEvent::Crossterm(e) => {
                handle_event(self, e, tx);
                true
            }
            AppEvent::PtyOutput { worktree_path } => {
                // Only redraw when the output is from the session currently
                // shown in the terminal panel. Background sessions updating
                // silently does not require a frame.
                self.state
                    .selected_worktree_path()
                    .map_or(false, |active| *active == worktree_path)
            }
            AppEvent::RepoLoaded(mut repo) => {
                // Preserve expansion state from any existing entry for this path.
                if let Some(existing) = self
                    .state
                    .repos
                    .iter()
                    .find(|r| r.config.path == repo.config.path)
                {
                    repo.is_expanded = existing.is_expanded;
                }

                match self
                    .state
                    .repos
                    .iter()
                    .position(|r| r.config.path == repo.config.path)
                {
                    Some(idx) => self.state.repos[idx] = repo,
                    None => self.state.repos.push(repo),
                }

                self.clamp_selection();
                true
            }
            AppEvent::PrsFetched { repo_path, prs } => {
                use crate::state::types::{PullRequest, PrState};
                if let Some(repo) = self
                    .state
                    .repos
                    .iter_mut()
                    .find(|r| r.config.path == repo_path)
                {
                    for wt in &mut repo.worktrees {
                        wt.pr = prs.iter().find(|p| p.head_ref == wt.branch).map(|p| {
                            PullRequest {
                                number: p.number,
                                title: p.title.clone(),
                                state: match p.state.as_str() {
                                    "MERGED" => PrState::Merged,
                                    "CLOSED" => PrState::Closed,
                                    _ => PrState::Open,
                                },
                                is_draft: p.is_draft,
                                url: p.url.clone(),
                            }
                        });
                    }
                }
                true
            }
            AppEvent::RefreshDone => {
                self.state.is_refreshing = false;
                // Persist fresh data so the next startup is instant.
                let repos = self.state.repos.clone();
                tokio::task::spawn_blocking(move || crate::cache::save(&repos));
                true
            }
            AppEvent::RefreshError(e) => {
                tracing::error!("Refresh error: {}", e);
                // Don't clear is_refreshing here — RefreshDone will arrive for the
                // overall cycle; this is a per-repo warning.
                true
            }
            AppEvent::WorktreeCreated {
                repo_path,
                worktree_path,
            } => {
                tracing::info!(
                    "Worktree created at {:?} for repo {:?}",
                    worktree_path,
                    repo_path
                );
                self.state.new_worktree_dialog = None;
                self.trigger_refresh(tx.clone());
                true
            }
            AppEvent::WorktreeCreateError(e) => {
                if let Some(dialog) = self.state.new_worktree_dialog.as_mut() {
                    dialog.is_creating = false;
                    dialog.error = Some(e);
                }
                true
            }
            AppEvent::WorktreeDeleted { repo_path: _, worktree_path } => {
                // Kill any PTY session for the deleted worktree.
                self.pty_manager.remove(&worktree_path);
                self.state.delete_worktree_dialog = None;
                self.trigger_refresh(tx.clone());
                true
            }
            AppEvent::WorktreeDeleteError(e) => {
                if let Some(dialog) = self.state.delete_worktree_dialog.as_mut() {
                    dialog.is_deleting = false;
                    dialog.error = Some(e);
                }
                true
            }
            AppEvent::RepoAdded(path) => {
                self.state.add_repo_dialog = None;
                self.config.repos.push(RepoConfig { path, name: None });
                if let Err(e) = save_config(&self.config) {
                    tracing::error!("Failed to save config: {}", e);
                }
                // Add the repo to state immediately so it appears before the refresh completes
                let new_repo = crate::state::types::Repository::new(
                    self.config.repos.last().unwrap().clone(),
                );
                self.state.repos.push(new_repo);
                self.trigger_refresh(tx.clone());
                true
            }
            AppEvent::RepoAddError(msg) => {
                if let Some(dialog) = self.state.add_repo_dialog.as_mut() {
                    dialog.is_adding = false;
                    dialog.error = Some(msg);
                }
                true
            }
            AppEvent::Tick => {
                // Redraw to update idle indicators, but only when sessions exist.
                self.pty_manager.has_any_sessions()
            }
            AppEvent::RefreshTick => {
                // See AppEvent::RefreshTick's doc comment for why this goes
                // through trigger_refresh rather than refreshing directly.
                self.trigger_refresh(tx.clone());
                false
            }
            AppEvent::Quit => {
                self.state.should_quit = true;
                false
            }
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste)?;
    terminal.show_cursor()?;
    Ok(())
}
