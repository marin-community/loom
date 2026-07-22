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

pub use client::{Attach, AttachInput, AttachOutput, Client, RelayEvent, RelayStream};
pub use supervisor::{run as supervise, SupervisorConfig};

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;

/// Which backend a supervisor runs. `Pty` is the historical terminal supervisor
/// (a PTY + vt100 screen, raw-byte attach); `Relay` is the ACP frame relay
/// (piped stdio, a durable on-disk frame spool, subscribe/ack/write). The two
/// share the process scaffolding — detached spawn, spec over stdin, control
/// socket, process-group kill — and differ only in what the core task owns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// PTY + vt100 screen (the default; what an omitted `mode` in the spec means).
    #[default]
    Pty,
    /// Piped stdio + frame spool (ACP agents).
    Relay,
}

/// Options for launching a session. Mirrors what loom passes to
/// `backend::new_session` plus the agent environment.
pub struct LaunchOptions<'a> {
    pub name: &'a str,
    pub cwd: &'a Path,
    /// The shell command the supervisor runs as `sh -c <script>`.
    pub script: &'a str,
    /// Environment for the child, applied via the process environment (execve),
    /// **not** by `export`-ing into [`Self::script`]. Delivered to the supervisor
    /// over stdin (see [`spawn_detached`]) and injected with `CommandBuilder::env`,
    /// so secret values never touch any process's argv. Inherited by the exec'd
    /// login shell and everything it spawns, exactly as `export` would.
    pub env: &'a [(&'a str, &'a str)],
    /// Start the supervisor and its child from an empty environment. The
    /// supplied `env` is then the complete child environment.
    pub env_clear: bool,
    pub cols: u16,
    pub rows: u16,
    /// Which backend to run. [`Mode::Pty`] (the default) keeps the historical
    /// terminal supervisor; [`Mode::Relay`] runs the ACP frame relay.
    pub mode: Mode,
    /// (Relay) Roll to a fresh spool segment once the active one exceeds this
    /// many bytes. `None` uses the supervisor default (a few MiB). Exposed mainly
    /// so tests can force rotation with a tiny value; ignored in PTY mode.
    pub segment_max_bytes: Option<u64>,
    /// The supervisor binary to re-exec as `<bin> supervise <spec>`. `None` uses
    /// the current executable — correct when the caller *is* the `tapestry`
    /// binary (the standalone CLI). A host like loom, whose `current_exe` is
    /// `loom`, must point this at the sibling `tapestry` binary, which carries
    /// the `supervise` subcommand.
    pub supervisor_bin: Option<&'a Path>,
}

/// Encode the supervisor launch spec exactly as [`spawn_detached`] sends it.
///
/// Alternative placement backends can carry the same stdin-only contract
/// without duplicating its shape or putting secret environment values on an
/// argv. The returned bytes are sensitive until the supervisor consumes them.
pub fn encode_launch_spec(
    opts: &LaunchOptions<'_>,
    environment_overrides: &[(&str, &str)],
) -> Result<Vec<u8>> {
    encode_launch_spec_with_ambient(opts, environment_overrides, std::env::vars())
}

fn encode_launch_spec_with_ambient(
    opts: &LaunchOptions<'_>,
    environment_overrides: &[(&str, &str)],
    ambient: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<u8>> {
    let mut env = BTreeMap::new();
    if !opts.env_clear {
        env.extend(ambient);
    }
    env.extend(
        opts.env
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    );
    env.extend(
        environment_overrides
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    );
    serde_json::to_vec(&LaunchSpec {
        name: opts.name.to_string(),
        cwd: opts.cwd.to_path_buf(),
        script: opts.script.to_string(),
        env: env.into_iter().collect(),
        env_clear: opts.env_clear,
        cols: opts.cols,
        rows: opts.rows,
        mode: opts.mode,
        segment_max_bytes: opts.segment_max_bytes,
    })
    .context("encoding tapestry launch spec")
}

/// Launch a session's supervisor as a **detached** process: it `setsid`s into
/// its own session, dropping its controlling terminal and stdio. With no
/// controlling terminal it ignores the SIGHUP its launcher's exit would
/// otherwise deliver, and once the launcher (loom) exits the kernel reparents it
/// to init — so the supervisor, and the agent under it, survive a loom restart.
/// Returns once the supervisor is accepting on its socket.
///
/// The supervisor is this very binary re-executed as `tapestry supervise -`; the
/// launch parameters travel as a single JSON blob **over stdin**, not on argv.
/// That keeps the spec's secret env values (tokens, API keys) out of the
/// supervisor's `/proc/<pid>/cmdline`, which is world-readable via `ps` for the
/// whole life of the long-running supervisor. It also means a script with spaces
/// or quotes needs no shell-escaping.
pub async fn spawn_detached(opts: &LaunchOptions<'_>) -> Result<()> {
    let exe = match opts.supervisor_bin {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe().context("resolving tapestry binary")?,
    };
    let spec = encode_launch_spec(opts, &[])?;

    let mut cmd = std::process::Command::new(&exe);
    if opts.env_clear {
        // The detached supervisor must resolve the same isolated socket root
        // as its parent, but it does not need the rest of loom's environment.
        let socket_env: Vec<_> = ["WEAVER_HOME", "WEAVER_TAPESTRY_DIR"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| (name, value)))
            .collect();
        cmd.env_clear();
        cmd.envs(socket_env);
    }
    // `-` tells the supervisor to read its JSON spec from stdin (see below); the
    // spec never appears on argv.
    cmd.arg("supervise")
        .arg("-")
        .stdin(Stdio::piped())
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
    let mut child = cmd.spawn().context("spawning detached supervisor")?;

    // Deliver the spec over the stdin pipe, then close it so the supervisor's
    // read hits EOF. The supervisor reads stdin to completion before any PTY
    // setup, so this small (<64 KiB) write never blocks. The child handle is
    // dropped without waiting — the supervisor is detached (setsid) and outlives
    // us on purpose.
    {
        use std::io::Write as _;
        let mut stdin = child
            .stdin
            .take()
            .context("detached supervisor stdin pipe missing")?;
        stdin
            .write_all(&spec)
            .context("writing launch spec to supervisor stdin")?;
    }

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
    #[serde(default)]
    pub env_clear: bool,
    pub cols: u16,
    pub rows: u16,
    /// Backend mode. Absent in specs written before relay mode existed, so it
    /// defaults to [`Mode::Pty`] — the historical behaviour, preserved for
    /// back-compat.
    #[serde(default)]
    pub mode: Mode,
    /// (Relay) Spool segment-rotation threshold in bytes; `None` = default.
    #[serde(default)]
    pub segment_max_bytes: Option<u64>,
}

impl From<LaunchSpec> for SupervisorConfig {
    fn from(s: LaunchSpec) -> Self {
        SupervisorConfig {
            name: s.name,
            cwd: s.cwd,
            script: s.script,
            env: s.env,
            env_clear: s.env_clear,
            cols: s.cols,
            rows: s.rows,
            mode: s.mode,
            segment_max_bytes: s.segment_max_bytes,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_launch_spec_preserves_the_stdin_contract() {
        let env = [("API_TOKEN", "secret-value")];
        let options = LaunchOptions {
            name: "session-1",
            cwd: Path::new("/workspace"),
            script: "agent --run",
            env: &env,
            env_clear: true,
            cols: 100,
            rows: 40,
            mode: Mode::Relay,
            segment_max_bytes: Some(1024),
            supervisor_bin: None,
        };
        let encoded = encode_launch_spec_with_ambient(&options, &[], []).unwrap();
        let decoded: LaunchSpec = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.name, "session-1");
        assert_eq!(decoded.cwd, Path::new("/workspace"));
        assert_eq!(decoded.script, "agent --run");
        assert_eq!(decoded.env, [("API_TOKEN".into(), "secret-value".into())]);
        assert!(decoded.env_clear);
        assert_eq!(decoded.mode, Mode::Relay);
    }

    #[test]
    fn encoded_launch_spec_preserves_ambient_env_with_explicit_precedence() {
        let env = [("EXPLICIT", "session")];
        let options = LaunchOptions {
            name: "session-1",
            cwd: Path::new("/workspace"),
            script: "true",
            env: &env,
            env_clear: false,
            cols: 80,
            rows: 24,
            mode: Mode::Pty,
            segment_max_bytes: None,
            supervisor_bin: None,
        };
        let ambient = [
            ("AMBIENT".to_string(), "inherited".to_string()),
            ("EXPLICIT".to_string(), "ambient".to_string()),
        ];
        let encoded = encode_launch_spec_with_ambient(
            &options,
            &[("WEAVER_API", "http://loom:7878")],
            ambient,
        )
        .unwrap();
        let decoded: LaunchSpec = serde_json::from_slice(&encoded).unwrap();
        let env: BTreeMap<_, _> = decoded.env.into_iter().collect();
        assert_eq!(env.get("AMBIENT").map(String::as_str), Some("inherited"));
        assert_eq!(env.get("EXPLICIT").map(String::as_str), Some("session"));
        assert_eq!(
            env.get("WEAVER_API").map(String::as_str),
            Some("http://loom:7878")
        );
    }

    #[test]
    fn encoded_launch_spec_drops_ambient_env_when_cleared() {
        let options = LaunchOptions {
            name: "session-1",
            cwd: Path::new("/workspace"),
            script: "true",
            env: &[],
            env_clear: true,
            cols: 80,
            rows: 24,
            mode: Mode::Pty,
            segment_max_bytes: None,
            supervisor_bin: None,
        };
        let ambient = [("AMBIENT".to_string(), "inherited".to_string())];
        let encoded = encode_launch_spec_with_ambient(&options, &[], ambient).unwrap();
        let decoded: LaunchSpec = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.env.is_empty());
    }
}
