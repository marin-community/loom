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
use crate::{backend, config, db, github, monitor, overlooker, web};
use weaver_core::branch as branch_mod;
use weaver_core::overlooker as ov;

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

/// Remove the state file on shutdown, but only if it still describes *this*
/// process.
///
/// A restart overlaps two servers on the same port: `loom server stop` drops the old
/// listener (freeing the port) the instant it signals shutdown, but the old
/// process keeps running until its in-flight connections drain — and the
/// dashboard's SSE/terminal streams can hold those open for a long time. In
/// that window the new server binds the freed port and writes its own
/// `loom.json`. If the departing server then deleted the file unconditionally
/// it would wipe the *successor's* state, leaving a live server with no state
/// file — exactly the "loom is running but loom.json is missing" failure on the
/// next `loom server restart`. So we only remove the file when the pid on disk is
/// still ours; otherwise a newer server owns it and we leave it be.
fn remove_state_if_ours(path: &std::path::Path, my_pid: u32) {
    let owner = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<ServerState>(&t).ok())
        .map(|s| s.pid);
    match owner {
        Some(pid) if pid == my_pid => {
            let _ = std::fs::remove_file(path);
        }
        Some(pid) => {
            tracing::info!(
                owner = pid,
                "leaving loom.json in place — a newer server owns it"
            );
        }
        None => {}
    }
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

    // Two independent startup adopt policies. The fleet-wide one recreates every
    // recoverable *ordinary* session's terminal, gated on `server.auto_adopt`. The
    // warm one recovers engine-managed (overlooker) sessions so a watcher resumes
    // its across-round memory after a restart — gated on its own
    // `overlooker.adopt_warm`, so warm infrastructure is recovered even when
    // ordinary sessions are deliberately left orphaned.
    if config::get_bool(&state.db, "server.auto_adopt", config::DEFAULT_AUTO_ADOPT).await {
        reconcile_sessions(&state).await;
    }
    if config::get_bool(
        &state.db,
        "overlooker.adopt_warm",
        config::DEFAULT_OVERLOOKER_ADOPT_WARM,
    )
    .await
    {
        reconcile_managed_sessions(&state).await;
    }

    tracing::info!(addr = %actual, pid = std::process::id(), "loom started");
    println!("loom listening on http://{actual}");
    let result = serve(state, listener).await;

    remove_state_if_ours(&path, std::process::id());
    match &result {
        Ok(()) => tracing::info!("loom stopped"),
        Err(e) => tracing::error!(error = %e, "loom stopped with error"),
    }
    result
}

/// On startup, adopt every recoverable *ordinary* session whose terminal is gone.
/// Engine-managed (warm) sessions are skipped here — they have their own adopt
/// policy in [`reconcile_managed_sessions`], gated on `overlooker.adopt_warm`.
async fn reconcile_sessions(state: &AppState) {
    let sessions = match session_mod::list(&state.db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("auto-adopt: listing sessions failed: {e}");
            return;
        }
    };
    for session in sessions {
        if session.managed_by.is_some() {
            continue;
        }
        if session_mod::is_terminal(&session.status) {
            continue;
        }
        if backend::has_session(&session.term_session).await {
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

/// Reconcile engine-managed (warm) overlooker sessions on startup, independent
/// of `server.auto_adopt`. For each managed session:
///
/// * its **owning overlooker is gone** (deleted) → the session is orphaned
///   infrastructure with no owner, so it is **archived** (terminal killed, worktree
///   removed), not adopted — it would never be surveyed or reused again;
/// * otherwise, if it is **non-terminal and its terminal is gone** → it is
///   **re-adopted** (terminal recreated, agent resumed) so the watcher resumes its
///   across-round memory. Its session id (and the overlooker's
///   `warm_session_id` linkage) is stable across the restart — adoption recreates
///   terminal for the *same* row rather than spawning a new session.
pub async fn reconcile_managed_sessions(state: &AppState) {
    let sessions = match session_mod::list_managed(&state.db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("warm-adopt: listing managed sessions failed: {e}");
            return;
        }
    };
    for session in sessions {
        let Some(owner_id) = session.managed_by.as_deref() else {
            continue;
        };
        // Distinguish "owner deleted" from "owner unreadable": only a definitive
        // miss archives the warm session. A transient DB error leaves it intact —
        // destroying a live watcher's session over a flaky read is far worse than
        // deferring its recovery to the next restart.
        let owner = match ov::get(&state.db, owner_id).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(
                    "warm-adopt: reading owner of managed session {} failed, leaving it intact: {e}",
                    session.id
                );
                continue;
            }
        };
        let Ok(Some(branch)) = branch_mod::get(&state.db, &session.branch_id).await else {
            continue;
        };

        // The owner is gone: this warm session is unreferenced infrastructure.
        // Tear it down rather than leave it dangling.
        if owner.is_none() {
            if session_mod::is_terminal(&session.status) {
                continue;
            }
            match web::archive(state, &session, &branch).await {
                Ok(_) => tracing::info!(
                    "warm-adopt: archived orphaned managed session {} (owner gone)",
                    session.id
                ),
                Err(e) => tracing::warn!(
                    "warm-adopt: could not archive managed session {}: {}",
                    session.id,
                    e.message()
                ),
            }
            continue;
        }

        // The owner is alive: re-adopt a recoverable warm session whose terminal is
        // gone, so the watcher resumes its across-round memory.
        if session_mod::is_terminal(&session.status) {
            continue;
        }
        if backend::has_session(&session.term_session).await {
            continue;
        }
        match web::adopt(state, &session, &branch).await {
            Ok(()) => tracing::info!("warm-adopt: adopted managed session {}", session.id),
            Err(e) => tracing::warn!(
                "warm-adopt: could not adopt managed session {}: {}",
                session.id,
                e.message()
            ),
        }
    }
}

pub async fn serve(state: AppState, listener: TcpListener) -> Result<()> {
    // Mint (or reuse) the machine-local token so loom's own same-host
    // subprocesses authenticate even when loopback trust is off. Best-effort:
    // a failure here must not stop the server coming up.
    match crate::auth::ensure_local_token(&state.db).await {
        Ok(_) => tracing::debug!("machine-local token ready"),
        Err(e) => tracing::warn!("could not prepare the machine-local token: {e}"),
    }
    tokio::spawn(monitor::run(state.clone()));
    // The GitHub poller is always spawned; it self-gates on the `github.poll`
    // setting and on `gh` being available, so it idles cheaply when GitHub
    // integration is off or unavailable.
    tokio::spawn(github::poll(state.clone()));
    // The Overlooker engine (timer + dispatcher). Always spawned; it self-gates
    // on the `overlooker.enabled` master switch, which is on by default, so a
    // default loom runs it. Turning the switch off idles it cheaply.
    tokio::spawn(overlooker::run(state.clone()));
    tracing::debug!("background tasks spawned (monitor, github poll, overlooker)");
    // `into_make_service_with_connect_info` surfaces the peer `SocketAddr` to the
    // auth middleware, which uses it to recognise (and optionally trust) loopback.
    axum::serve(
        listener,
        web::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
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

    fn write_state(path: &std::path::Path, pid: u32) {
        let state = ServerState {
            pid,
            addr: "127.0.0.1:7878".to_string(),
            started_at: "2026-05-20T12:00:00.000Z".to_string(),
        };
        std::fs::write(path, serde_json::to_string(&state).unwrap()).unwrap();
    }

    #[test]
    fn shutdown_removes_only_our_own_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.json");

        // Our own file is removed on shutdown.
        write_state(&path, 1000);
        remove_state_if_ours(&path, 1000);
        assert!(!path.exists(), "a server should remove its own state file");

        // A successor's file (different pid) is left untouched — this is the
        // restart race: the departing server must not wipe the new server's
        // loom.json.
        write_state(&path, 2000);
        remove_state_if_ours(&path, 1000);
        assert!(
            path.exists(),
            "a departing server must not delete a newer server's state file"
        );
        assert_eq!(read_state_at(&path).unwrap().pid, 2000);
    }

    #[test]
    fn shutdown_tolerates_a_missing_or_corrupt_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.json");

        // Missing file: nothing to do, no panic.
        remove_state_if_ours(&path, 1000);

        // Corrupt file: left in place rather than blindly deleted.
        std::fs::write(&path, "not json").unwrap();
        remove_state_if_ours(&path, 1000);
        assert!(path.exists(), "an unparseable state file is left untouched");
    }

    fn read_state_at(path: &std::path::Path) -> Option<ServerState> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }
}
