//! Human-facing server-log endpoints: a snapshot of recent log lines and a
//! live SSE tail, backed by the in-process ring buffer ([`crate::logs`]). These
//! sit in the authenticated router. Admins see the operator log verbatim; user
//! roles receive the same diagnostic stream with known and token-shaped secrets
//! redacted. See docs/loom-ui or Settings → Diagnostics.

use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::auth::Principal;
use crate::db::Db;
use crate::logs::{self, LogLine, LogRedactor};
use crate::tasks::{self, TaskRecord};

use super::{ApiResult, AppState};

#[derive(Debug, Deserialize)]
pub(super) struct LogsQuery {
    /// Most-recent lines to return. Defaults to 500; clamped to the buffer size.
    limit: Option<usize>,
}

/// `GET /api/logs` — a snapshot of the most recent server log lines, oldest
/// first. The UI loads this once, then follows [`logs_stream`] for new lines.
pub(super) async fn logs_snapshot(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<Vec<LogLine>>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    let redactor = log_redactor(&st.db, &principal).await?;
    let lines = logs::buffer()
        .snapshot(limit)
        .into_iter()
        .map(|line| redact_line(&redactor, line))
        .collect();
    Ok(Json(lines))
}

/// `GET /api/logs/stream` — server log lines as they are emitted (SSE). The
/// browser authenticates with the `loom_session` cookie (EventSource can't set
/// headers), exactly like the session-events stream.
pub(super) async fn logs_stream(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    let redactor = log_redactor(&st.db, &principal).await?;
    let stream = BroadcastStream::new(logs::buffer().subscribe()).filter_map(move |result| {
        // A lagged subscriber yields Err; skip the gap (the client can re-snapshot).
        let line = redact_line(&redactor, result.ok()?);
        Some(Ok(sse::Event::default()
            .event("log")
            .json_data(&line)
            .unwrap_or_default()))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub(super) async fn log_redactor(db: &Db, principal: &Principal) -> ApiResult<Option<LogRedactor>> {
    if principal.is_admin() {
        return Ok(None);
    }

    let mut secrets: Vec<String> = sqlx::query_scalar("SELECT token FROM user_github_tokens")
        .fetch_all(db)
        .await?;
    secrets.extend(
        sqlx::query_scalar::<_, String>("SELECT value FROM repo_env")
            .fetch_all(db)
            .await?,
    );
    secrets.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM profile_env
             WHERE instr(upper(name), 'TOKEN') > 0
                OR instr(upper(name), 'SECRET') > 0
                OR instr(upper(name), 'PASSWORD') > 0
                OR instr(upper(name), 'API_KEY') > 0
                OR instr(upper(name), 'PRIVATE_KEY') > 0",
        )
        .fetch_all(db)
        .await?,
    );
    secrets.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings
             WHERE instr(lower(key), 'token') > 0
                OR instr(lower(key), 'secret') > 0
                OR instr(lower(key), 'password') > 0
                OR instr(lower(key), 'private_key') > 0
                OR instr(lower(key), 'api_key') > 0
             UNION ALL
             SELECT value FROM deployment_settings
             WHERE instr(lower(key), 'token') > 0
                OR instr(lower(key), 'secret') > 0
                OR instr(lower(key), 'password') > 0
                OR instr(lower(key), 'private_key') > 0
                OR instr(lower(key), 'api_key') > 0",
        )
        .fetch_all(db)
        .await?,
    );
    secrets.extend(
        std::env::vars()
            .filter(|(name, _)| secret_environment_name(name))
            .map(|(_, value)| value),
    );
    Ok(Some(LogRedactor::new(secrets)))
}

pub(super) fn redact_line(redactor: &Option<LogRedactor>, line: LogLine) -> LogLine {
    match redactor {
        Some(redactor) => redactor.redact(line),
        None => line,
    }
}

fn secret_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "PRIVATE_KEY", "API_KEY"]
        .iter()
        .any(|marker| name.contains(marker))
}

/// A small "what am I looking at" status blob for the debug panel: build and
/// image identity plus process identity, so both deploys and restarts are
/// attributable.
#[derive(Debug, Serialize)]
pub(super) struct ServerStatus {
    version: &'static str,
    build_revision: &'static str,
    build_profile: &'static str,
    /// Digest-pinned image reference when a container deployment supplies one.
    image: Option<String>,
    pid: u32,
    /// When this process started capturing logs (≈ process start), RFC3339.
    started_at: String,
}

/// `GET /api/status` — build and process identity for human users.
pub(super) async fn server_status() -> Json<ServerStatus> {
    Json(ServerStatus {
        version: env!("CARGO_PKG_VERSION"),
        build_revision: env!("LOOM_BUILD_REVISION"),
        build_profile: env!("LOOM_BUILD_PROFILE"),
        image: std::env::var("LOOM_IMAGE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        pid: std::process::id(),
        started_at: logs::buffer().started_at().to_string(),
    })
}

/// `GET /api/tasks` — recent detached background tasks (the GitHub-trigger
/// launches that run off the webhook request), newest first. Human-only, same as
/// the log endpoints — a task label names a repo/issue a user can act on.
pub(super) async fn tasks_snapshot() -> Json<Vec<TaskRecord>> {
    Json(tasks::registry().snapshot())
}
