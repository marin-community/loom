//! Tapestry — a barebones per-session terminal supervisor, the native
//! replacement for `tmux attach` in loom.
//!
//! Each session is one tiny **detached supervisor process** that owns the
//! agent's PTY, runs a vt100 screen emulator, and serves a unix control socket.
//! Because the supervisor's lifetime is independent of `loom serve`, restarting
//! loom leaves a live agent untouched — the same recovery property tmux gives —
//! while the interactive surface streams *raw PTY bytes*, so an attached xterm
//! owns its own scrollback, selection, and search instead of rendering a
//! re-rendered tmux screen.
//!
//! ## Surface
//!
//! * [`spawn_detached`] — launch a session's supervisor, reparented to init.
//! * [`Client`] — drive a session: [`Client::is_alive`], [`Client::capture`],
//!   [`Client::send`], [`Client::resize`], [`Client::kill`], and the interactive
//!   [`Client::attach`].
//! * [`list_sessions`] — every session with a live supervisor.
//! * [`supervisor::run`] — the supervisor event loop (the `tapestry supervise`
//!   binary entry point; tests call it in-process).

pub mod client;
pub mod paths;
pub mod protocol;
pub mod supervisor;

pub use client::{Attach, Client};
pub use supervisor::{run as supervise, SupervisorConfig};

use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;

/// Options for launching a session. Mirrors what loom already passes to
/// `tmux::new_session` plus the agent environment.
pub struct LaunchOptions<'a> {
    pub name: &'a str,
    pub cwd: &'a Path,
    /// `sh -c <script>` — the same contract as `tmux new-session … sh -c`.
    pub script: &'a str,
    pub env: &'a [(&'a str, &'a str)],
    pub cols: u16,
    pub rows: u16,
}

/// Launch a session's supervisor as a **detached** process: it `setsid`s into
/// its own session, drops its controlling terminal and stdio, and reparents to
/// init, so it survives the caller (loom) exiting. Returns once the supervisor
/// is accepting on its socket.
///
/// The supervisor is this very binary re-executed as `tapestry supervise`; the
/// launch parameters travel as a single JSON argument so a script with spaces or
/// quotes needs no shell-escaping.
pub async fn spawn_detached(opts: &LaunchOptions<'_>) -> Result<()> {
    let exe = std::env::current_exe().context("resolving tapestry binary")?;
    let spec = serde_json::json!({
        "name": opts.name,
        "cwd": opts.cwd,
        "script": opts.script,
        "env": opts.env,
        "cols": opts.cols,
        "rows": opts.rows,
    });

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("supervise")
        .arg(spec.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // setsid in the child so it has no controlling terminal and survives the
    // parent's process group going away.
    unsafe {
        cmd.pre_exec(|| {
            // Detach from the controlling terminal / parent session.
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("spawning detached supervisor")?;

    // Wait (bounded) for the socket to come up so callers can immediately drive
    // the session, mirroring how `tmux new-session` returns only once the
    // session exists.
    let socket = paths::socket_path(opts.name);
    for _ in 0..200 {
        if tokio::net::UnixStream::connect(&socket).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("supervisor for {} did not come up within 5s", opts.name)
}

/// The launch parameters carried in the `supervise` argument. Owned mirror of
/// [`LaunchOptions`] for (de)serialization across the exec boundary.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct LaunchSpec {
    pub name: String,
    pub cwd: std::path::PathBuf,
    pub script: String,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

impl From<LaunchSpec> for SupervisorConfig {
    fn from(s: LaunchSpec) -> Self {
        SupervisorConfig {
            name: s.name,
            cwd: s.cwd,
            script: s.script,
            env: s.env,
            cols: s.cols,
            rows: s.rows,
        }
    }
}

/// Every session that currently has a *live* supervisor (socket present and
/// answering). The `tmux list-sessions` analogue.
pub async fn list_sessions() -> Vec<String> {
    let mut live = Vec::new();
    for name in paths::list_socket_names() {
        if Client::is_alive(&name).await {
            live.push(name);
        }
    }
    live
}
