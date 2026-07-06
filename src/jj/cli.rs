use std::io;
use std::process::{Command as StdCommand, Output};
use anyhow::{anyhow, bail, Result};

fn friendly_spawn_error(args: &[&str], e: io::Error) -> anyhow::Error {
    if e.kind() == io::ErrorKind::NotFound {
        anyhow!(
            "jj CLI not found on PATH — install Jujutsu (https://jj-vcs.github.io) to manage this repository"
        )
    } else {
        anyhow!("Failed to run jj {}: {}", args.join(" "), e)
    }
}

fn finish(args: &[&str], output: Output) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `jj` synchronously. Use from blocking contexts (e.g. inside
/// `spawn_blocking`, mirroring how `git2` calls are made there).
pub fn run_jj(args: &[&str]) -> Result<String> {
    let output = StdCommand::new("jj")
        .args(args)
        .output()
        .map_err(|e| friendly_spawn_error(args, e))?;
    finish(args, output)
}

/// Run `jj` asynchronously. Use from async contexts (e.g. spawned via
/// `tokio::spawn`, mirroring `git::worktree`'s use of `tokio::process::Command`).
pub async fn run_jj_async(args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("jj")
        .args(args)
        .output()
        .await
        .map_err(|e| friendly_spawn_error(args, e))?;
    finish(args, output)
}
