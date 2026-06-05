//! Server bootstrap: opens the database, spawns background tasks, and serves
//! the axum app.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::events::EventBus;
use crate::session as session_mod;
use crate::web::AppState;
use crate::{config, db, monitor, tmux, web};
use weaver_core::branch as branch_mod;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerState {
    pub pid: u32,
    pub addr: String,
    pub started_at: String,
}

pub fn state_path() -> PathBuf {
    db::weaver_home().join("loom.json")
}

pub fn read_state() -> Option<ServerState> {
    let text = std::fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&text).ok()
}

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
    let state = AppState {
        db,
        bus: EventBus::new(),
        addr: actual.to_string(),
    };

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

    if config::get_bool(&state.db, "server.auto_adopt", config::DEFAULT_AUTO_ADOPT).await {
        reconcile_sessions(&state).await;
    }

    tracing::info!(addr = %actual, pid = std::process::id(), "loom started");
    println!("loom listening on http://{actual}");
    let result = serve(state, listener).await;

    std::fs::remove_file(&path).ok();
    match &result {
        Ok(()) => tracing::info!("loom stopped"),
        Err(e) => tracing::error!(error = %e, "loom stopped with error"),
    }
    result
}

/// On startup, adopt every recoverable session whose tmux is gone.
async fn reconcile_sessions(state: &AppState) {
    let sessions = match session_mod::list(&state.db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("auto-adopt: listing sessions failed: {e}");
            return;
        }
    };
    for session in sessions {
        if session_mod::is_terminal(&session.status) {
            continue;
        }
        if tmux::has_session(&session.tmux_session).await {
            continue;
        }
        let Ok(Some(branch)) = branch_mod::get(&state.db, &session.branch_id).await else {
            continue;
        };
        match web::adopt(state, &session, &branch).await {
            Ok(()) => tracing::info!("auto-adopt: adopted session {}", session.id),
            Err(e) => tracing::warn!(
                "auto-adopt: could not adopt session {}: {}",
                session.id,
                e.message()
            ),
        }
    }
}

pub async fn serve(state: AppState, listener: TcpListener) -> Result<()> {
    tokio::spawn(monitor::run(state.clone()));
    tracing::debug!("background tasks spawned (monitor)");
    axum::serve(listener, web::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

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
}
