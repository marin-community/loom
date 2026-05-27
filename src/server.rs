//! Server bootstrap: opens the database, spawns background tasks, and serves
//! the axum app.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::events::EventBus;
use crate::web::AppState;
use crate::{config, db, monitor, summary, tmux, web, workspace};

/// On-disk record of the running server, written by [`run`] once the listener
/// is bound. CLI subcommands read it to find the server's pid and address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerState {
    /// Process id of the `weaver server run` process.
    pub pid: u32,
    /// host:port the server bound to.
    pub addr: String,
    /// ISO-8601 timestamp of when the server started.
    pub started_at: String,
}

/// Path to the server state file under the weaver home directory.
pub fn state_path() -> PathBuf {
    db::weaver_home().join("server.json")
}

/// Read the server state file, if it exists and parses.
pub fn read_state() -> Option<ServerState> {
    let text = std::fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Run the weaver server bound to `addr` (e.g. `127.0.0.1:7878`).
///
/// This is the production entrypoint: it writes a [`ServerState`] file once the
/// listener is bound and best-effort removes it after a graceful shutdown.
pub async fn run(addr: &str) -> Result<()> {
    let socket: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid bind address '{addr}'"))?;
    let listener = TcpListener::bind(socket).await.map_err(|e| {
        tracing::error!(addr = %socket, error = %e, "failed to bind listener");
        anyhow::Error::new(e).context(format!("binding {socket}"))
    })?;
    let actual = listener.local_addr()?;
    tracing::debug!(addr = %actual, "listener bound");

    let db = db::connect(&db::default_db_path()).await?;
    tracing::debug!("database connected");
    let state = AppState {
        db,
        bus: EventBus::new(),
        addr: actual.to_string(),
    };

    // Record the running server so CLI subcommands can find it. This is
    // intentionally kept out of `serve()`, which must stay side-effect-free
    // for the integration test.
    let server_state = ServerState {
        pid: std::process::id(),
        addr: actual.to_string(),
        started_at: db::now_iso(),
    };
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    match serde_json::to_string_pretty(&server_state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("could not write server state file: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize server state: {e}"),
    }

    // Reboot recovery: tmux sessions do not survive a reboot, but the DB rows
    // and worktrees do. When `server.auto_adopt` is enabled, recreate the tmux
    // session for every recoverable workspace before serving. When disabled,
    // do nothing — the monitor will mark them `orphaned` on its first tick.
    if config::get_bool(&state.db, "server.auto_adopt", config::DEFAULT_AUTO_ADOPT).await {
        reconcile_workspaces(&state).await;
    }

    tracing::info!(addr = %actual, pid = std::process::id(), "weaver server started");
    println!("weaver server listening on http://{actual}");
    let result = serve(state, listener).await;

    // Best-effort cleanup after a graceful shutdown.
    std::fs::remove_file(&path).ok();
    match &result {
        Ok(()) => tracing::info!("weaver server stopped"),
        Err(e) => tracing::error!(error = %e, "weaver server stopped with error"),
    }
    result
}

/// Adopt every non-terminal workspace that has lost its tmux session, reusing
/// the same logic as the `POST /workspaces/{id}/adopt` handler. Best-effort:
/// a failure to adopt one workspace is logged and does not stop the others.
async fn reconcile_workspaces(state: &AppState) {
    let workspaces = match workspace::list(&state.db).await {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("auto-adopt: listing workspaces failed: {e}");
            return;
        }
    };
    for ws in workspaces {
        if workspace::is_terminal(&ws.status) {
            continue;
        }
        if tmux::has_session(&ws.tmux_session).await {
            continue;
        }
        match web::adopt(state, &ws).await {
            Ok(()) => tracing::info!("auto-adopt: adopted workspace {} ({})", ws.id, ws.name),
            Err(e) => tracing::warn!("auto-adopt: could not adopt {}: {}", ws.id, e.message()),
        }
    }
}

/// Spawn background tasks and serve the app on an existing listener. Exposed
/// for integration tests; deliberately free of filesystem side effects.
pub async fn serve(state: AppState, listener: TcpListener) -> Result<()> {
    tokio::spawn(monitor::run(state.clone()));
    tokio::spawn(summary::run(state.clone()));
    tracing::debug!("background tasks spawned (monitor, summary)");
    axum::serve(listener, web::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Resolve when the process should drain and exit: on Ctrl-C (SIGINT) or on
/// SIGTERM (`kill <pid>`, systemd `stop`), whichever arrives first.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // If the handler can't be installed, never resolve this branch so
            // Ctrl-C still works.
            Err(e) => {
                tracing::warn!("could not install SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_state_roundtrips_through_json() {
        let state = ServerState {
            pid: 4242,
            addr: "127.0.0.1:7878".to_string(),
            started_at: "2026-05-20T12:00:00.000Z".to_string(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: ServerState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, parsed);
    }

    #[test]
    fn server_state_deserializes_expected_shape() {
        let json = r#"{"pid":99,"addr":"0.0.0.0:9000","started_at":"2026-01-01T00:00:00.000Z"}"#;
        let parsed: ServerState = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.pid, 99);
        assert_eq!(parsed.addr, "0.0.0.0:9000");
        assert_eq!(parsed.started_at, "2026-01-01T00:00:00.000Z");
    }
}
