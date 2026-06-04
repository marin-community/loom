//! Thin async wrapper over the `tmux` binary.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Output;
use tokio::process::Command;

/// Server-socket flags (`-L <name>`) prepended to every tmux invocation when
/// `WEAVER_TMUX_SOCKET` is set. This pins tmux to a dedicated server so the test
/// suite never touches the user's real sessions; unset in production, tmux uses
/// its default socket and behaviour is unchanged. `-L` is a server option, so it
/// must precede the command.
pub fn socket_args() -> Vec<String> {
    match std::env::var("WEAVER_TMUX_SOCKET") {
        Ok(s) if !s.is_empty() => vec!["-L".to_string(), s],
        _ => Vec::new(),
    }
}

async fn raw(args: &[&str]) -> Result<Output> {
    Command::new("tmux")
        .args(socket_args())
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

/// An exact-match, session-scoped `-t` target for a session name.
///
/// The leading `=` forces an exact (not prefix) match, so a name containing
/// `:`/`.`/`%` can't accidentally retarget another session/window/pane. The
/// trailing `:` scopes the target to the *session* — this matters because
/// window/pane-context commands (`capture-pane`, `set-option`, `display-message`)
/// otherwise parse a bare `=name` as a *window* name and fail with
/// "no such window". The `=name:` form is correct for both those and the
/// session-context commands (`has-session`, `kill-session`, `attach-session`).
pub fn exact(name: &str) -> String {
    format!("={name}:")
}

/// Whether a session with exactly this name exists.
pub async fn has_session(name: &str) -> bool {
    raw(&["has-session", "-t", &exact(name)])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create a detached session running `script` via `sh -c` in `cwd`.
pub async fn new_session(name: &str, cwd: &Path, script: &str) -> Result<()> {
    let cwd = cwd.to_string_lossy();
    run(&[
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        &cwd,
        "sh",
        "-c",
        script,
    ])
    .await?;
    // Let an attached client drive the window size: `window-size latest` makes
    // the window track the most-recently-active client (so a browser PTY can
    // resize it via SIGWINCH) instead of clamping to the smallest attached
    // client. Set defensively in case the user's tmux.conf overrode the
    // default. Best-effort: a failure here only affects terminal sizing.
    let t = exact(name);
    let _ = run(&["set-option", "-t", &t, "window-size", "latest"]).await;
    let _ = run(&["set-option", "-t", &t, "aggressive-resize", "on"]).await;
    tracing::info!(session = name, cwd = %cwd, "tmux session created");
    Ok(())
}

/// Capture the session's pane. `history` extra scrollback lines (0 = visible screen only).
pub async fn capture(name: &str, history: usize) -> Result<String> {
    let start;
    let target = exact(name);
    let mut args = vec!["capture-pane", "-p", "-t", &target];
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
    let _ = raw(&["kill-session", "-t", &exact(name)]).await;
    tracing::info!(session = name, "tmux session killed");
    Ok(())
}

pub async fn list_sessions() -> Result<Vec<String>> {
    let out = run(&["list-sessions", "-F", "#{session_name}"]).await?;
    Ok(out.lines().map(|s| s.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_is_session_scoped() {
        assert_eq!(exact("weaver-abc123"), "=weaver-abc123:");
    }

    /// Guards the target form against a regression: a bare `=name` works for
    /// `has-session`/`kill-session` but makes `capture-pane`/`set-option` fail
    /// with "no such window", which would silently break the monitor and
    /// browser-driven sizing. Requires `tmux` on PATH (as the rest of the suite
    /// already does).
    #[tokio::test]
    async fn capture_and_set_option_work_through_exact() {
        // Pin tmux to a throwaway server so this never touches the user's real
        // sessions; killing the lone session below lets it exit-empty.
        std::env::set_var(
            "WEAVER_TMUX_SOCKET",
            format!("weaver-unittest-{}", std::process::id()),
        );
        let name = format!("weaver-tmuxtest-{}", std::process::id());
        // Best-effort cleanup of a stale session from a previous aborted run.
        let _ = kill_session(&name).await;

        let dir = std::env::temp_dir();
        new_session(&name, &dir, "echo TMUX_EXACT_MARKER; exec sleep 30")
            .await
            .expect("new_session should succeed");
        assert!(
            has_session(&name).await,
            "session should exist after create"
        );

        // set-option through the session-scoped target must succeed (rc=0).
        run(&["set-option", "-t", &exact(&name), "window-size", "latest"])
            .await
            .expect("set-option through exact() target should succeed");

        // capture-pane through the same target must return Ok and see the marker.
        let mut seen = false;
        for _ in 0..40 {
            match capture(&name, 0).await {
                Ok(screen) => {
                    if screen.contains("TMUX_EXACT_MARKER") {
                        seen = true;
                        break;
                    }
                }
                Err(e) => panic!("capture through exact() target failed: {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(seen, "capture should see the marker printed in the pane");

        kill_session(&name).await.unwrap();
        assert!(
            !has_session(&name).await,
            "session should be gone after kill"
        );
    }
}
