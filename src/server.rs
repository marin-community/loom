//! Server bootstrap: opens the database, spawns background tasks, and serves
//! the axum app.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::net::TcpListener;

use crate::events::EventBus;
use crate::web::AppState;
use crate::{db, monitor, summary, web};

/// Run the weaver server bound to `addr` (e.g. `127.0.0.1:7878`).
pub async fn run(addr: &str) -> Result<()> {
    let socket: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid bind address '{addr}'"))?;
    let listener = TcpListener::bind(socket)
        .await
        .with_context(|| format!("binding {socket}"))?;
    let actual = listener.local_addr()?;

    let db = db::connect(&db::default_db_path()).await?;
    let state = AppState {
        db,
        bus: EventBus::new(),
        addr: actual.to_string(),
    };

    tracing::info!("weaver listening on http://{actual}");
    println!("weaver server listening on http://{actual}");
    serve(state, listener).await
}

/// Spawn background tasks and serve the app on an existing listener. Exposed
/// for integration tests.
pub async fn serve(state: AppState, listener: TcpListener) -> Result<()> {
    tokio::spawn(monitor::run(state.clone()));
    tokio::spawn(summary::run(state.clone()));
    axum::serve(listener, web::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
