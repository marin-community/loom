//! Operator-facing server-log endpoints: a snapshot of recent log lines and a
//! live SSE tail, backed by the in-process ring buffer ([`crate::logs`]). These
//! sit in the authenticated router, so only an approved operator can read them —
//! server logs can carry tokens injected into agents. See docs/loom-ui or the
//! Settings → Logs panel.

use std::convert::Infallible;

use axum::extract::Query;
use axum::response::sse::{self, KeepAlive, Sse};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::logs::{self, LogLine};
use crate::tasks::{self, TaskRecord};

#[derive(Debug, Deserialize)]
pub(super) struct LogsQuery {
    /// Most-recent lines to return. Defaults to 500; clamped to the buffer size.
    limit: Option<usize>,
}

/// `GET /api/logs` — a snapshot of the most recent server log lines, oldest
/// first. The UI loads this once, then follows [`logs_stream`] for new lines.
pub(super) async fn logs_snapshot(Query(q): Query<LogsQuery>) -> Json<Vec<LogLine>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    Json(logs::buffer().snapshot(limit))
}

/// `GET /api/logs/stream` — server log lines as they are emitted (SSE). The
/// browser authenticates with the `loom_session` cookie (EventSource can't set
/// headers), exactly like the session-events stream.
pub(super) async fn logs_stream() -> Sse<impl Stream<Item = Result<sse::Event, Infallible>>> {
    let stream = BroadcastStream::new(logs::buffer().subscribe()).filter_map(|result| {
        // A lagged subscriber yields Err; skip the gap (the client can re-snapshot).
        let line = result.ok()?;
        Some(Ok(sse::Event::default()
            .event("log")
            .json_data(&line)
            .unwrap_or_default()))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// A small "what am I looking at" status blob for the debug panel — enough to
/// tell whether a redeploy landed (pid + start time change on restart).
#[derive(Debug, Serialize)]
pub(super) struct ServerStatus {
    version: &'static str,
    pid: u32,
    /// When this process started capturing logs (≈ process start), RFC3339.
    started_at: String,
}

/// `GET /api/status` — build version + pid + start time (operator-only).
pub(super) async fn server_status() -> Json<ServerStatus> {
    Json(ServerStatus {
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        started_at: logs::buffer().started_at().to_string(),
    })
}

/// `GET /api/tasks` — recent detached background tasks (the GitHub-trigger
/// launches that run off the webhook request), newest first. Operator-only, same
/// as the log endpoints — a task label names a repo/issue an operator can act on.
pub(super) async fn tasks_snapshot() -> Json<Vec<TaskRecord>> {
    Json(tasks::registry().snapshot())
}
