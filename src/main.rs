mod app;
mod cache;
mod config;
mod error;
mod events;
mod git;
mod github;
mod jj;
mod keybindings;
mod pty;
mod state;
mod ui;
mod vcs;

use std::panic;
use anyhow::{Context, Result};
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use crossterm::execute;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, EnvFilter};

use crate::app::App;
use crate::config::load_config;
use crate::state::types::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging();
    setup_panic_handler();

    tracing::info!("Starting gitopiary");

    let config = load_config().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config: {}, using defaults", e);
        crate::config::Config::default()
    });

    tracing::info!("Loaded config with {} repos", config.repos.len());

    let keybindings = crate::keybindings::Keybindings::from_config(&config.keybindings)
        .context("Invalid keybinding configuration")?;

    // Seed initial state from the on-disk cache so worktrees are visible
    // immediately, before the background git refresh completes.
    let cache = cache::load();
    let initial_repos = config
        .repos
        .iter()
        .map(|rc| cache::hydrate_repo(rc.clone(), &cache))
        .collect();

    tracing::info!(
        "Loaded {} repos from cache",
        cache.repos.len(),
    );

    let state = AppState::new(initial_repos, keybindings);
    let app = App::new(state, config);

    app.run().await?;

    Ok(())
}

fn setup_logging() {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("gitopiary");

    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, "gitopiary.log");

    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(file_appender)
        .with_ansi(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber).ok();
}

fn setup_panic_handler() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Always restore terminal on panic
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}
