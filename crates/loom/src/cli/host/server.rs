//! `loom server` — the daemon's own lifecycle, plus `loom open`.
//!
//! Every command here is host-local: it starts, stops, or points a browser at
//! a process on this machine, and none of it is expressible as an operation.

use crate::client;
use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;

/// Subcommands under `loom server` — the daemon lifecycle.
#[derive(Subcommand)]
pub enum ServerCmd {
    /// Run the server in the foreground (REST API + Vue UI + monitor loop).
    ///
    /// Blocks until interrupted — the form to run under a process supervisor
    /// (systemd, Docker) or while developing/testing. `loom server start` runs
    /// this same process in the background.
    Run {
        #[arg(long)]
        addr: Option<String>,
    },
    /// Start the server in the background (daemonize) and wait for it to be healthy.
    Start,
    /// Stop the background server.
    Stop,
    /// Stop and re-start the background server.
    Restart,
    /// Show the running server's status.
    Status,
}

/// Why a server must not start here, if it must not.
///
/// A Loom session reaches its host loom over the API; its `WEAVER_HOME` is the
/// host's own home, so a server started inside one opens that database and
/// those supervisor sockets a second time. The machine then runs two monitors,
/// two Slack clients, and two sets of lifecycle operations on the same rows —
/// and an operation owned by the session's process dies the instant it tears
/// that session's supervisor down, stranding the transition it published.
///
/// The signal that a home already belongs to a loom is its `loom.json` state
/// file. A private `WEAVER_HOME` has none, so the documented way to exercise
/// loom by hand (`WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0`)
/// still works.
pub(crate) fn nested_server_refusal(
    session_id: Option<&str>,
    home: &std::path::Path,
) -> Option<String> {
    let session_id = session_id.filter(|id| !id.is_empty())?;
    let state = home.join("loom.json");
    if !state.exists() {
        return None;
    }
    Some(format!(
        "refusing to start: this is Loom session {session_id}, and {} already belongs to a running loom. A second server on one home races the host's monitor, Slack client, and session teardown. Run `WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0` for an isolated instance.",
        home.display()
    ))
}

/// Dispatch the `loom server <verb>` daemon-lifecycle subcommands.
pub async fn run_server(cmd: ServerCmd) -> Result<()> {
    if matches!(cmd, ServerCmd::Run { .. } | ServerCmd::Start) {
        if let Some(refusal) = nested_server_refusal(
            std::env::var("LOOM_SESSION_ID").ok().as_deref(),
            &crate::db::weaver_home(),
        ) {
            bail!("{refusal}");
        }
    }
    match cmd {
        ServerCmd::Run { addr } => {
            init_tracing();
            let addr = crate::endpoint::bind_addr(addr.as_deref());
            crate::server::run(&addr).await
        }
        ServerCmd::Start => cmd_start().await,
        ServerCmd::Stop => cmd_stop().await,
        ServerCmd::Restart => cmd_restart().await,
        ServerCmd::Status => cmd_status().await,
    }
}

pub(crate) fn init_tracing() {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("loom=info,weaver_core=info,tower_http=warn"));
    // Registry-of-layers so the ring-buffer capture (the in-browser log viewer)
    // runs *alongside* the existing stdout output — `docker compose logs` is
    // unchanged; the buffer tees. The one `EnvFilter` gates both layers.
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(crate::logs::layer())
        .init();
}

pub(crate) fn server_base() -> String {
    crate::endpoint::base_url()
}

pub(crate) async fn server_is_up(base: &str) -> bool {
    let url = format!("{base}/api/health");
    match reqwest::get(&url).await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

pub(crate) fn format_uptime(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {s}s")
    } else {
        format!("{s}s")
    }
}

pub(crate) fn uptime_secs(started_at: &str) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    Some((chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds())
}

pub(crate) async fn wait_for_health(base: &str, want: bool, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if server_is_up(base).await == want {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

pub(crate) async fn cmd_status() -> Result<()> {
    let base = server_base();
    if !server_is_up(&base).await {
        println!("loom: not running");
        return Ok(());
    }
    match crate::server::read_state() {
        Some(state) => {
            print!(
                "loom: running at http://{}  (pid {})",
                state.addr, state.pid
            );
            match uptime_secs(&state.started_at) {
                Some(secs) => println!("  up {}", format_uptime(secs)),
                None => println!(),
            }
        }
        None => println!("loom: running at {base}  (no state file)"),
    }
    Ok(())
}

pub(crate) async fn cmd_start() -> Result<()> {
    let base = server_base();
    if server_is_up(&base).await {
        println!("loom already running at {base}");
        return Ok(());
    }
    spawn_server().await
}

pub(crate) async fn spawn_server() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("locating the loom binary")?;
    let addr = crate::endpoint::bind_addr(None);
    let home = crate::db::weaver_home();
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let log_path = home.join("loom.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let log_err = log.try_clone()?;

    let mut command = std::process::Command::new(&exe);
    command
        .args(["server", "run"])
        .arg("--addr")
        .arg(&addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0);
    let child = command.spawn().context("spawning `loom server run`")?;
    drop(child);

    let base = format!("http://{addr}");
    if wait_for_health(&base, true, std::time::Duration::from_secs(10)).await {
        println!("loom started at {base}");
        Ok(())
    } else {
        bail!(
            "loom did not come up within 10s — check the log at {}",
            log_path.display()
        )
    }
}

pub(crate) async fn cmd_stop() -> Result<()> {
    let base = server_base();
    if !server_is_up(&base).await {
        println!("loom is not running");
        return Ok(());
    }
    let state = crate::server::read_state().ok_or_else(|| {
        anyhow!(
            "loom is running but {} is missing or unreadable — stop it manually",
            crate::server::state_path().display()
        )
    })?;
    let status = std::process::Command::new("kill")
        .arg(state.pid.to_string())
        .status()
        .context("failed to run `kill`")?;
    if !status.success() {
        bail!(
            "`kill {}` failed — the process may already be gone",
            state.pid
        );
    }
    if wait_for_health(&base, false, std::time::Duration::from_secs(10)).await {
        println!("loom stopped (pid {})", state.pid);
        Ok(())
    } else {
        bail!("loom (pid {}) did not stop within 10s", state.pid)
    }
}

pub(crate) async fn cmd_restart() -> Result<()> {
    let base = server_base();
    if server_is_up(&base).await {
        cmd_stop().await?;
    }
    spawn_server().await
}

pub async fn cmd_open() -> Result<()> {
    let client = client::default()?;
    let url = client.base().to_string();
    println!("opening {url}");
    if std::process::Command::new("xdg-open")
        .arg(&url)
        .status()
        .is_err()
    {
        println!("open it manually: {url}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_may_not_start_a_server_on_the_host_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("loom.json"), "{}").unwrap();
        assert_eq!(
            nested_server_refusal(Some("bej3oxrv"), home.path()).unwrap(),
            format!(
                "refusing to start: this is Loom session bej3oxrv, and {} already belongs to a running loom. A second server on one home races the host's monitor, Slack client, and session teardown. Run `WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0` for an isolated instance.",
                home.path().display()
            )
        );
    }
    #[test]
    fn a_private_weaver_home_keeps_hand_testing_available() {
        // The session env always carries a WEAVER_HOME — the host's; an isolated
        // one is safe only because no loom lives in it yet.
        let home = tempfile::tempdir().unwrap();
        assert!(nested_server_refusal(Some("bej3oxrv"), home.path()).is_none());
    }
    #[test]
    fn the_host_server_is_not_a_session() {
        // The host's own restart finds its predecessor's loom.json and must
        // still start; only a session is refused.
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("loom.json"), "{}").unwrap();
        assert!(nested_server_refusal(None, home.path()).is_none());
        assert!(nested_server_refusal(Some(""), home.path()).is_none());
    }
    #[test]
    fn format_uptime_picks_a_sensible_granularity() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(-5), "0s");
        assert_eq!(format_uptime(42), "42s");
        assert_eq!(format_uptime(90), "1m 30s");
        assert_eq!(format_uptime(3_600), "1h 0m");
        assert_eq!(format_uptime(3_661), "1h 1m");
        assert_eq!(format_uptime(90_061), "1d 1h 1m");
    }
}
