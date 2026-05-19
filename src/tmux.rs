//! Thin async wrapper over the `tmux` binary.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Output;
use tokio::process::Command;

async fn raw(args: &[&str]) -> Result<Output> {
    Command::new("tmux")
        .args(args)
        .output()
        .await
        .context("failed to spawn tmux")
}

async fn run(args: &[&str]) -> Result<String> {
    let out = raw(args).await?;
    if !out.status.success() {
        bail!(
            "tmux {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Whether a session with exactly this name exists.
pub async fn has_session(name: &str) -> bool {
    raw(&["has-session", "-t", &format!("={name}")])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create a detached session running `script` via `sh -c` in `cwd`.
pub async fn new_session(name: &str, cwd: &Path, script: &str) -> Result<()> {
    let cwd = cwd.to_string_lossy();
    run(&[
        "new-session", "-d", "-s", name, "-c", &cwd, "sh", "-c", script,
    ])
    .await?;
    Ok(())
}

/// Type `text` into the session's active pane, followed by Enter.
pub async fn send_text(name: &str, text: &str) -> Result<()> {
    run(&["send-keys", "-t", name, "-l", "--", text]).await?;
    run(&["send-keys", "-t", name, "Enter"]).await?;
    Ok(())
}

/// Capture the session's pane. `history` extra scrollback lines (0 = visible screen only).
pub async fn capture(name: &str, history: usize) -> Result<String> {
    let start;
    let mut args = vec!["capture-pane", "-p", "-t", name];
    if history > 0 {
        start = format!("-{history}");
        args.push("-S");
        args.push(&start);
    }
    run(&args).await
}

pub async fn kill_session(name: &str) -> Result<()> {
    // Ignore "session not found"; the goal is just for it to be gone.
    let _ = raw(&["kill-session", "-t", &format!("={name}")]).await;
    Ok(())
}

pub async fn list_sessions() -> Result<Vec<String>> {
    let out = run(&["list-sessions", "-F", "#{session_name}"]).await?;
    Ok(out.lines().map(|s| s.to_string()).collect())
}
