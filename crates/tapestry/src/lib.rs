//! Tapestry — a barebones per-session terminal supervisor; loom's native
//! terminal backend.
//!
//! Each session is one tiny **detached supervisor process** that owns the
//! agent's PTY, runs a vt100 screen emulator, and serves a unix control socket.
//! Because the supervisor's lifetime is independent of `loom server run`, restarting
//! loom leaves a live agent untouched (the recovery property loom relies on),
//! while the interactive surface streams *raw PTY bytes*, so an attached xterm
//! owns its own scrollback, selection, and search rather than a server-rendered
//! screen.
//!
//! ## Surface
//!
//! * [`spawn_detached`] — launch a session's supervisor in its own session, so
//!   it outlives the launcher.
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

pub use client::{Attach, AttachInput, AttachOutput, Client};
pub use supervisor::{run as supervise, SupervisorConfig};

use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;

/// Options for launching a session. Mirrors what loom passes to
/// `backend::new_session` plus the agent environment.
pub struct LaunchOptions<'a> {
    pub name: &'a str,
    pub cwd: &'a Path,
    /// The shell command the supervisor runs as `sh -c <script>`.
    pub script: &'a str,
    pub env: &'a [(&'a str, &'a str)],
    pub cols: u16,
    pub rows: u16,
    /// The supervisor binary to re-exec as `<bin> supervise <spec>`. `None` uses
    /// the current executable — correct when the caller *is* the `tapestry`
    /// binary (the standalone CLI). A host like loom, whose `current_exe` is
    /// `loom`, must point this at the sibling `tapestry` binary, which carries
    /// the `supervise` subcommand.
    pub supervisor_bin: Option<&'a Path>,
}

/// Launch a session's supervisor as a **detached** process: it `setsid`s into
/// its own session, dropping its controlling terminal and stdio. With no
/// controlling terminal it ignores the SIGHUP its launcher's exit would
/// otherwise deliver, and once the launcher (loom) exits the kernel reparents it
/// to init — so the supervisor, and the agent under it, survive a loom restart.
/// Returns once the supervisor is accepting on its socket.
///
/// The supervisor is this very binary re-executed as `tapestry supervise`; the
/// launch parameters travel as a single JSON argument so a script with spaces or
/// quotes needs no shell-escaping.
pub async fn spawn_detached(opts: &LaunchOptions<'_>) -> Result<()> {
    let exe = match opts.supervisor_bin {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe().context("resolving tapestry binary")?,
    };
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
    // setsid in the child so it leads a new session with no controlling
    // terminal, and so survives the parent's process group going away. setsid
    // only fails (EPERM) if the caller already leads a process group — which a
    // freshly-forked child never does — but surface the error rather than swallow
    // it, so a launch into a half-detached state fails loudly instead of silently
    // staying attached to loom's session.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("spawning detached supervisor")?;

    // Wait (bounded) for the socket to come up so callers can drive the session
    // as soon as this returns.
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
/// answering).
pub async fn list_sessions() -> Vec<String> {
    let mut live = Vec::new();
    for name in paths::list_socket_names() {
        if Client::is_alive(&name).await {
            live.push(name);
        }
    }
    live
}
