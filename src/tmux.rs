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
    tracing::debug!(?args, "running tmux");
    let out = raw(args).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        tracing::warn!(
            args = %args.join(" "),
            code = out.status.code().unwrap_or(-1),
            stderr = %truncate(stderr.trim(), 500),
            stdout = %truncate(stdout.trim(), 500),
            "tmux failed"
        );
        bail!("tmux {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Truncate a string to `max` chars for log output, appending an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push_str("...[truncated]");
        t
    }
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
    tracing::info!(session = name, cwd = %cwd, "tmux session created");
    Ok(())
}

/// Type `text` into the session's active pane, followed by Enter.
pub async fn send_text(name: &str, text: &str) -> Result<()> {
    run(&["send-keys", "-t", name, "-l", "--", text]).await?;
    run(&["send-keys", "-t", name, "Enter"]).await?;
    Ok(())
}

/// Send named keys (e.g. `Escape`, `C-c`) to the session's active pane.
///
/// Unlike [`send_text`], the arguments are interpreted by tmux as key names
/// rather than literal text, and no trailing Enter is appended.
pub async fn send_keys(name: &str, keys: &[&str]) -> Result<()> {
    let mut args = vec!["send-keys", "-t", name];
    args.extend_from_slice(keys);
    run(&args).await?;
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
    tracing::debug!(session = name, "running tmux kill-session");
    let _ = raw(&["kill-session", "-t", &format!("={name}")]).await;
    tracing::info!(session = name, "tmux session killed");
    Ok(())
}

pub async fn list_sessions() -> Result<Vec<String>> {
    let out = run(&["list-sessions", "-F", "#{session_name}"]).await?;
    Ok(out.lines().map(|s| s.to_string()).collect())
}
