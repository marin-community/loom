//! The terminal-management seam.
//!
//! Loom's programmatic terminal operations — create a session, check liveness,
//! capture the screen, type into it, kill it — go through here rather than
//! poking the supervisor directly, so the call sites read uniformly and the
//! `tapestry`-specific glue (binary resolution, key-name → bytes) lives in one
//! place.
//!
//! Every session is a [`tapestry`] supervisor: a per-session detached PTY process
//! that streams raw PTY bytes, so an attached xterm owns its own scrollback,
//! selection, and search. Its lifetime is independent of `loom server run`, so a loom
//! restart leaves running terminals untouched — the recovery property that keeps
//! agents alive across restarts.

use anyhow::Result;

/// Whether a session with this name has a live supervisor.
pub async fn has_session(name: &str) -> bool {
    tapestry::Client::is_alive(name).await
}

/// Create a detached session running `script` via `sh -c` in `cwd`.
pub async fn new_session(name: &str, cwd: &std::path::Path, script: &str) -> Result<()> {
    tapestry::spawn_detached(&tapestry::LaunchOptions {
        name,
        cwd,
        script,
        // The launch script already bakes the agent env in as `export`
        // statements (see agent::launch_script).
        env: &[],
        cols: 80,
        rows: 24,
        supervisor_bin: tapestry_bin().as_deref(),
    })
    .await
}

/// Render the session's screen to text; `history` extra scrollback lines.
pub async fn capture(name: &str, history: usize) -> Result<String> {
    let mut c = tapestry::Client::connect(name).await?;
    c.capture(history as u32).await
}

/// Type `text` into the session verbatim (the `send-keys -l` analogue). No
/// trailing newline; pair with [`send_enter`] to submit.
pub async fn send_literal(name: &str, text: &str) -> Result<()> {
    let mut c = tapestry::Client::connect(name).await?;
    c.send(text.as_bytes()).await
}

/// Send a single named key (e.g. `Enter`, `Escape`) to the session.
pub async fn send_key(name: &str, key: &str) -> Result<()> {
    let mut c = tapestry::Client::connect(name).await?;
    c.send(key_bytes(key)).await
}

/// Submit the current input — a bare `Enter`.
pub async fn send_enter(name: &str) -> Result<()> {
    send_key(name, "Enter").await
}

/// Kill the session. Best-effort: a missing supervisor means already gone.
pub async fn kill_session(name: &str) -> Result<()> {
    if let Ok(mut c) = tapestry::Client::connect(name).await {
        match c.kill().await {
            Ok(()) => tracing::info!(name = %name, "terminal killed"),
            Err(e) => tracing::warn!(name = %name, error = %e, "failed to kill terminal"),
        }
    }
    Ok(())
}

/// Every session with a live supervisor.
pub async fn list_sessions() -> Result<Vec<String>> {
    Ok(tapestry::list_sessions().await)
}

/// The `tapestry` supervisor binary, resolved as a sibling of the running `loom`
/// executable (they ship together), so the detached supervisor is the real
/// `tapestry` binary rather than `loom` (whose `current_exe` lacks a `supervise`
/// subcommand). `None` falls back to `tapestry` on `PATH`. An explicit
/// `WEAVER_TAPESTRY_BIN` overrides both — used by the integration tests, whose
/// `loom` and `tapestry` are built into the same target dir.
fn tapestry_bin() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("WEAVER_TAPESTRY_BIN") {
        return Some(std::path::PathBuf::from(p));
    }
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|d| d.join("tapestry"))
        .filter(|p| p.exists())
}

/// Translate the small set of key names loom uses into the raw bytes a PTY
/// expects. Unknown names fall through as their literal text.
fn key_bytes(key: &str) -> &[u8] {
    match key {
        "Enter" => b"\r",
        "Escape" => b"\x1b",
        "Tab" => b"\t",
        "Space" => b" ",
        "BSpace" => b"\x7f",
        other => other.as_bytes(),
    }
}
