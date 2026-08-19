//! Human-facing server-log endpoints: a snapshot of recent log lines and a
//! live SSE tail, backed by the in-process ring buffer ([`crate::logs`]). These
//! sit in the authenticated router. Admins see the operator log verbatim; user
//! roles receive the same diagnostic stream with known and token-shaped secrets
//! redacted. See docs/loom-ui or Settings → Diagnostics.

use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::Extension;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use weaver_api::operations::diagnostics as diagnostics_operations;
use weaver_api::operations::logs as log_operations;
use weaver_api::operations::tasks as task_operations;
use weaver_api::TaskView;

use crate::auth::Principal;
use crate::db::Db;
use crate::logs::{self, LogLine, LogRedactor};
use crate::tasks::{self, TaskRecord};

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppState};

/// The `tasks` bundle (one read-only operation over the in-memory background
/// task ring buffer) plus two operations bound here alongside the legacy
/// handlers they port: `logs.list` (the snapshot counterpart of
/// `logs.stream`) and `diagnostics.status` (next to `server_status`; its
/// sibling `diagnostics.get` is bound in `web/diagnostics.rs`, next to the
/// `diagnostics` handler it ports).
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<task_operations::list::List, _, _>(list_tasks),
        register::<log_operations::list::List, _, _>(list_logs_operation),
        register::<diagnostics_operations::status::Status, _, _>(status_operation),
    ]
}

/// `tasks.list`, `crates/weaver-api/src/operations/tasks/list.rs`. [`TaskView`]
/// mirrors [`TaskRecord`] field-for-field but is a distinct wire type owned by
/// weaver-api, so the mapping is spelled out rather than assumed.
fn task_view(record: TaskRecord) -> TaskView {
    TaskView {
        id: record.id,
        kind: record.kind,
        label: record.label,
        state: record.state,
        detail: record.detail,
        started_at: record.started_at,
        finished_at: record.finished_at,
    }
}

pub(super) async fn list_tasks(
    _context: OperationContext,
    _input: task_operations::list::Input,
) -> ApiResult<Vec<TaskView>> {
    Ok(tasks::registry()
        .snapshot()
        .into_iter()
        .map(task_view)
        .collect())
}

/// Shared by [`logs_snapshot`] and `logs.list`: the redacted tail of recent
/// log lines, oldest first.
async fn logs_snapshot_core(
    st: &AppState,
    principal: &Principal,
    limit: Option<i64>,
) -> ApiResult<Vec<LogLine>> {
    let limit = limit.unwrap_or(500).clamp(1, 2000) as usize;
    let redactor = log_redactor(&st.db, principal).await?;
    Ok(logs::buffer()
        .snapshot(limit)
        .into_iter()
        .map(|line| redact_line(&redactor, line))
        .collect())
}

fn log_line_view(line: LogLine) -> log_operations::list::LogLineView {
    log_operations::list::LogLineView {
        seq: line.seq,
        ts: line.ts,
        level: line.level,
        target: line.target,
        message: line.message,
    }
}

/// `logs.list` — the twin of [`logs_snapshot`].
async fn list_logs_operation(
    context: OperationContext,
    input: log_operations::list::Input,
) -> ApiResult<log_operations::list::Output> {
    let lines = logs_snapshot_core(&context.state, &context.principal, input.limit).await?;
    Ok(lines.into_iter().map(log_line_view).collect())
}

/// The `logs.stream` operation — server log lines as they are emitted (SSE).
/// The browser authenticates with the `loom_session` cookie (EventSource can't
/// set headers), exactly like the session-events stream.
pub(super) async fn logs_stream(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<log_operations::stream::Input>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    super::encodings::authorized::<log_operations::stream::Stream>(&st, &principal, input).await?;
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
/// attributable. Shared by [`server_status`] and `diagnostics.status`.
fn status_view() -> diagnostics_operations::status::Output {
    diagnostics_operations::status::Output {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_revision: env!("LOOM_BUILD_REVISION").to_string(),
        build_profile: env!("LOOM_BUILD_PROFILE").to_string(),
        image: std::env::var("LOOM_IMAGE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        pid: std::process::id(),
        started_at: logs::buffer().started_at().to_string(),
    }
}

/// `diagnostics.status` — the twin of [`server_status`].
async fn status_operation(
    _context: OperationContext,
    _input: diagnostics_operations::status::Input,
) -> ApiResult<diagnostics_operations::status::Output> {
    Ok(status_view())
}
