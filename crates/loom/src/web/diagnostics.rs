//! Operational health, Prometheus metrics, and the authenticated diagnostics snapshot.
//!
//! Everything here is derived from durable control-plane state. Labels are
//! restricted to bounded operational dimensions; identities, paths, branch and
//! session keys, token identifiers, and raw error text never enter metrics or
//! diagnostics payloads.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::Result;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use sqlx::FromRow;
use weaver_api::operations::diagnostics as diagnostics_operations;
use weaver_api::{
    DiagnosticFederation, DiagnosticProblemSummary, DiagnosticProfileCapacity, DiagnosticRunCount,
    DiagnosticRunFailure, DiagnosticRunSummary, DiagnosticSessionCount, DiagnosticsView,
    MigrationStreamView, ReadinessView,
};

use crate::auth::Principal;
use crate::db::Db;

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

const LOCAL_RUNNER_POOL: &str = "local";
const METRICS_CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[derive(FromRow)]
struct SessionCountRow {
    status: String,
    class: String,
    profile: String,
    protocol: String,
    count: i64,
}

#[derive(FromRow)]
struct ProfileCapacityRow {
    profile: String,
    revision: i64,
    maximum: i64,
    active: i64,
}

#[derive(FromRow)]
struct RunCountRow {
    status: String,
    source: String,
    service_tag: String,
    profile: String,
    count: i64,
}

#[derive(FromRow)]
struct RunFailureRow {
    source: String,
    profile: String,
    outcome: Option<String>,
    updated_at: String,
}

#[derive(FromRow)]
struct ProblemRow {
    status: String,
    class: String,
    profile: String,
    protocol: String,
    count: i64,
    latest_activity_at: Option<String>,
}

#[derive(FromRow)]
struct FederationRow {
    name: String,
    provider: String,
    audience: String,
    service_tag: String,
    profiles_json: String,
    created_at: String,
    updated_at: String,
}

fn bounded_status(value: &str) -> &str {
    match value {
        "created" | "launching" | "running" | "orphaned" | "done" | "error" | "archived" => value,
        _ => "other",
    }
}

fn bounded_class(value: &str) -> &str {
    match value {
        "interactive" | "automation" => value,
        _ => "other",
    }
}

fn bounded_protocol(value: &str) -> &str {
    match value {
        "terminal" | "acp" => value,
        _ => "other",
    }
}

fn bounded_run_status(value: &str) -> &str {
    match value {
        "creating" | "running" | "completed" | "failed" | "cancelled" => value,
        _ => "other",
    }
}

fn bounded_source(value: &str) -> &str {
    match value {
        "actions" | "ops" | "grafana" => value,
        _ => "other",
    }
}

fn label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

async fn migration_states(db: &Db) -> Result<Vec<MigrationStreamView>> {
    let streams = [
        (
            "core",
            "schema_migrations",
            weaver_core::migrations::latest_version(),
        ),
        (
            "loom",
            "loom_schema_migrations",
            crate::db::latest_migration_version(),
        ),
    ];
    let mut states = Vec::with_capacity(streams.len());
    for (stream, table, expected) in streams {
        // Both identifiers are compile-time literals. SQLite cannot bind table
        // names, so keep this list closed rather than accepting caller input.
        let query = format!("SELECT COALESCE(MAX(version), 0), COUNT(*) FROM {table}");
        let (current, applied): (i64, i64) = sqlx::query_as(&query).fetch_one(db).await?;
        states.push(MigrationStreamView {
            stream: stream.to_string(),
            current,
            expected,
            applied,
            ready: current == expected && applied == expected,
        });
    }
    Ok(states)
}

async fn readiness_snapshot(db: &Db) -> Result<ReadinessView> {
    let _: i64 = sqlx::query_scalar("SELECT 1").fetch_one(db).await?;
    let migrations = migration_states(db).await?;
    let ready = migrations.iter().all(|stream| stream.ready);
    Ok(ReadinessView {
        status: if ready { "ready" } else { "not_ready" }.to_string(),
        database: true,
        migrations,
        degraded: Vec::new(),
    })
}

/// Process-level liveness. Kept separate from database checks so a wedged DB
/// does not cause the process supervisor to restart an otherwise diagnosable
/// API in a loop.
pub(super) async fn liveness() -> &'static str {
    "ok"
}

/// Database + migration readiness. Optional remote runner capacity will be
/// added to `degraded`, not folded into the top-level readiness decision.
pub(super) async fn readiness(State(st): State<AppState>) -> Response {
    match readiness_snapshot(&st.db).await {
        Ok(view) => {
            let status = if view.status == "ready" {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (status, Json(view)).into_response()
        }
        Err(error) => {
            tracing::error!(error = %format!("{error:?}"), "readiness database check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ReadinessView {
                    status: "not_ready".to_string(),
                    database: false,
                    migrations: Vec::new(),
                    degraded: vec!["database unavailable".to_string()],
                }),
            )
                .into_response()
        }
    }
}

async fn session_counts(db: &Db) -> Result<Vec<DiagnosticSessionCount>> {
    let rows = sqlx::query_as::<_, SessionCountRow>(
        "SELECT status, class, profile, protocol, COUNT(*) AS count
         FROM sessions
         GROUP BY status, class, profile, protocol
         ORDER BY status, class, profile, protocol",
    )
    .fetch_all(db)
    .await?;
    let mut counts = BTreeMap::new();
    for row in rows {
        *counts
            .entry((
                bounded_status(&row.status).to_string(),
                bounded_class(&row.class).to_string(),
                row.profile,
                bounded_protocol(&row.protocol).to_string(),
            ))
            .or_insert(0) += row.count;
    }
    Ok(counts
        .into_iter()
        .map(
            |((status, class, profile, protocol), count)| DiagnosticSessionCount {
                status,
                class,
                profile,
                protocol,
                runner_pool: LOCAL_RUNNER_POOL.to_string(),
                count,
            },
        )
        .collect())
}

async fn profile_capacity(db: &Db) -> Result<Vec<DiagnosticProfileCapacity>> {
    let rows = sqlx::query_as::<_, ProfileCapacityRow>(
        "SELECT p.name AS profile, p.revision, p.max_concurrent AS maximum,
                COUNT(s.id) AS active
         FROM profiles p
         LEFT JOIN sessions s ON s.profile = p.name
            AND s.status NOT IN ('done', 'error', 'archived')
         GROUP BY p.name, p.revision, p.max_concurrent
         ORDER BY p.name",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let maximum = (row.maximum > 0).then_some(row.maximum);
            DiagnosticProfileCapacity {
                profile: row.profile,
                revision: row.revision,
                active: row.active,
                maximum,
                available: maximum.map(|limit| (limit - row.active).max(0)),
            }
        })
        .collect())
}

async fn automation_runs(db: &Db) -> Result<DiagnosticRunSummary> {
    let rows = sqlx::query_as::<_, RunCountRow>(
        "SELECT status, source, service_tag, profile, COUNT(*) AS count
         FROM automation_runs
         GROUP BY status, source, service_tag, profile
         ORDER BY status, source, service_tag, profile",
    )
    .fetch_all(db)
    .await?;
    let mut grouped_counts = BTreeMap::new();
    for row in rows {
        *grouped_counts
            .entry((
                bounded_run_status(&row.status).to_string(),
                bounded_source(&row.source).to_string(),
                row.service_tag,
                row.profile,
            ))
            .or_insert(0) += row.count;
    }
    let counts = grouped_counts
        .into_iter()
        .map(
            |((status, source, service_tag, profile), count)| DiagnosticRunCount {
                status,
                source,
                service_tag,
                profile,
                count,
            },
        )
        .collect();
    let stale_before = (chrono::Utc::now() - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let stale_creating = sqlx::query_scalar(
        "SELECT COUNT(*) FROM automation_runs WHERE status = 'creating' AND updated_at <= ?",
    )
    .bind(stale_before)
    .fetch_one(db)
    .await?;
    let recent_failures = sqlx::query_as::<_, RunFailureRow>(
        "SELECT source, profile, outcome, updated_at
         FROM automation_runs WHERE status = 'failed'
         ORDER BY updated_at DESC LIMIT 20",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|row| DiagnosticRunFailure {
        source: bounded_source(&row.source).to_string(),
        profile: row.profile,
        outcome: row
            .outcome
            .as_deref()
            .map(bounded_run_status)
            .map(str::to_string),
        updated_at: row.updated_at,
    })
    .collect();
    Ok(DiagnosticRunSummary {
        counts,
        stale_creating,
        recent_failures,
    })
}

async fn problems(db: &Db) -> Result<Vec<DiagnosticProblemSummary>> {
    let rows = sqlx::query_as::<_, ProblemRow>(
        "SELECT s.status, s.class, s.profile, s.protocol, COUNT(*) AS count,
                MAX(COALESCE(s.last_activity_at, b.updated_at, s.created_at)) AS latest_activity_at
         FROM sessions s
         LEFT JOIN branches b ON b.id = s.branch_id
         WHERE s.status IN ('orphaned', 'error')
         GROUP BY s.status, s.class, s.profile, s.protocol
         ORDER BY s.status, s.class, s.profile, s.protocol",
    )
    .fetch_all(db)
    .await?;
    let mut grouped = BTreeMap::new();
    for row in rows {
        let entry = grouped
            .entry((
                bounded_status(&row.status).to_string(),
                bounded_class(&row.class).to_string(),
                row.profile,
                bounded_protocol(&row.protocol).to_string(),
            ))
            .or_insert((0, None::<String>));
        entry.0 += row.count;
        if row.latest_activity_at.as_ref() > entry.1.as_ref() {
            entry.1 = row.latest_activity_at;
        }
    }
    Ok(grouped
        .into_iter()
        .map(
            |((status, class, profile, protocol), (count, latest_activity_at))| {
                DiagnosticProblemSummary {
                    status,
                    class,
                    profile,
                    protocol,
                    runner_pool: LOCAL_RUNNER_POOL.to_string(),
                    count,
                    latest_activity_at,
                }
            },
        )
        .collect())
}

async fn federations(db: &Db) -> Result<Vec<DiagnosticFederation>> {
    let rows = sqlx::query_as::<_, FederationRow>(
        "SELECT name, provider, audience, service_tag, profiles_json,
                created_at, updated_at
         FROM federation_mappings ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|row| {
            let profiles = serde_json::from_str(&row.profiles_json)?;
            Ok(DiagnosticFederation {
                name: row.name,
                provider: row.provider,
                audience: row.audience,
                service_tag: row.service_tag,
                profiles,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
        .collect()
}

async fn snapshot(db: &Db) -> Result<DiagnosticsView> {
    Ok(DiagnosticsView {
        sessions: session_counts(db).await?,
        profiles: profile_capacity(db).await?,
        automation_runs: automation_runs(db).await?,
        problems: problems(db).await?,
        migrations: migration_states(db).await?,
        federations: federations(db).await?,
    })
}

async fn metric_snapshot(db: &Db) -> Result<DiagnosticsView> {
    Ok(DiagnosticsView {
        sessions: session_counts(db).await?,
        profiles: profile_capacity(db).await?,
        automation_runs: automation_runs(db).await?,
        problems: Vec::new(),
        migrations: migration_states(db).await?,
        federations: Vec::new(),
    })
}

/// The `diagnostics` bundle's REST-reachable half — `diagnostics.get`. Its
/// sibling `diagnostics.status` is bound in `web/logview.rs`, next to the
/// legacy `server_status` handler it ports.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<diagnostics_operations::get::Get, _, _>(
        diagnostics_operation,
    )]
}

/// `diagnostics.get` — the twin of [`diagnostics`]. `actor = User` already
/// excludes every non-human principal, so the inline `is_human()` check the
/// legacy handler makes is not repeated here (same as `auth.automation_token`
/// dropping its inline `is_admin()` check once `actor = Admin` said the same
/// thing structurally).
async fn diagnostics_operation(
    context: OperationContext,
    _input: diagnostics_operations::get::Input,
) -> ApiResult<DiagnosticsView> {
    Ok(snapshot(&context.state.db).await?)
}

fn append_help(output: &mut String, name: &str, help: &str, metric_type: &str) {
    writeln!(output, "# HELP {name} {help}").expect("writing a String cannot fail");
    writeln!(output, "# TYPE {name} {metric_type}").expect("writing a String cannot fail");
}

async fn render_metrics(db: &Db, view: &DiagnosticsView) -> Result<String> {
    let mut output = String::new();
    append_help(
        &mut output,
        "loom_sessions_current",
        "Current durable sessions by bounded control-plane dimensions.",
        "gauge",
    );
    for row in &view.sessions {
        if row.status == "archived" {
            continue;
        }
        writeln!(
            output,
            "loom_sessions_current{{status=\"{}\",class=\"{}\",profile=\"{}\",protocol=\"{}\",runner_pool=\"{}\"}} {}",
            label_value(&row.status),
            label_value(&row.class),
            label_value(&row.profile),
            label_value(&row.protocol),
            label_value(&row.runner_pool),
            row.count
        )
        .expect("writing a String cannot fail");
    }

    append_help(
        &mut output,
        "loom_sessions_created_total",
        "Durable session rows created, including archived history.",
        "counter",
    );
    let mut created: BTreeMap<(&str, &str, &str), i64> = BTreeMap::new();
    for row in &view.sessions {
        *created
            .entry((&row.profile, &row.class, &row.runner_pool))
            .or_default() += row.count;
    }
    for ((profile, class, runner_pool), count) in created {
        writeln!(
            output,
            "loom_sessions_created_total{{profile=\"{}\",class=\"{}\",runner_pool=\"{}\"}} {}",
            label_value(profile),
            label_value(class),
            label_value(runner_pool),
            count
        )
        .expect("writing a String cannot fail");
    }

    append_help(
        &mut output,
        "loom_session_turns_total",
        "Completed agent turns retained in durable session state.",
        "counter",
    );
    let turns: Vec<(String, i64)> = sqlx::query_as(
        "SELECT profile, COALESCE(SUM(turn_count), 0) FROM sessions GROUP BY profile ORDER BY profile",
    )
    .fetch_all(db)
    .await?;
    for (profile, count) in turns {
        writeln!(
            output,
            "loom_session_turns_total{{profile=\"{}\"}} {count}",
            label_value(&profile)
        )
        .expect("writing a String cannot fail");
    }

    append_help(
        &mut output,
        "loom_profile_revision",
        "Current revision of each configured launch profile.",
        "gauge",
    );
    append_help(
        &mut output,
        "loom_profile_capacity",
        "Profile capacity by used, limit, and available state; unlimited profiles omit limit and available.",
        "gauge",
    );
    for profile in &view.profiles {
        let name = label_value(&profile.profile);
        writeln!(
            output,
            "loom_profile_revision{{profile=\"{name}\"}} {}",
            profile.revision
        )
        .expect("writing a String cannot fail");
        writeln!(
            output,
            "loom_profile_capacity{{profile=\"{name}\",state=\"used\"}} {}",
            profile.active
        )
        .expect("writing a String cannot fail");
        if let (Some(limit), Some(available)) = (profile.maximum, profile.available) {
            writeln!(
                output,
                "loom_profile_capacity{{profile=\"{name}\",state=\"limit\"}} {limit}"
            )
            .expect("writing a String cannot fail");
            writeln!(
                output,
                "loom_profile_capacity{{profile=\"{name}\",state=\"available\"}} {available}"
            )
            .expect("writing a String cannot fail");
        }
    }

    append_help(
        &mut output,
        "loom_automation_runs_current",
        "Durable automation runs by status, source, service, and profile.",
        "gauge",
    );
    for run in &view.automation_runs.counts {
        writeln!(
            output,
            "loom_automation_runs_current{{status=\"{}\",source=\"{}\",service=\"{}\",profile=\"{}\"}} {}",
            label_value(&run.status),
            label_value(&run.source),
            label_value(&run.service_tag),
            label_value(&run.profile),
            run.count
        )
        .expect("writing a String cannot fail");
    }
    append_help(
        &mut output,
        "loom_automation_runs_stale_creating",
        "Automation runs left in creating state for more than five minutes.",
        "gauge",
    );
    writeln!(
        output,
        "loom_automation_runs_stale_creating {}",
        view.automation_runs.stale_creating
    )
    .expect("writing a String cannot fail");

    append_help(
        &mut output,
        "loom_migration_version",
        "Applied and expected schema migration versions.",
        "gauge",
    );
    append_help(
        &mut output,
        "loom_migration_ready",
        "Whether a schema migration stream is at the version expected by this process.",
        "gauge",
    );
    for migration in &view.migrations {
        let stream = label_value(&migration.stream);
        writeln!(
            output,
            "loom_migration_version{{stream=\"{stream}\",state=\"applied\"}} {}",
            migration.current
        )
        .expect("writing a String cannot fail");
        writeln!(
            output,
            "loom_migration_version{{stream=\"{stream}\",state=\"expected\"}} {}",
            migration.expected
        )
        .expect("writing a String cannot fail");
        writeln!(
            output,
            "loom_migration_ready{{stream=\"{stream}\"}} {}",
            i32::from(migration.ready)
        )
        .expect("writing a String cannot fail");
    }
    output.push_str("# EOF\n");
    Ok(output)
}

/// Public scrape surface. It contains aggregates only; the richer inventory is
/// human-gated at `/api/diagnostics`.
pub(super) async fn metrics(State(st): State<AppState>) -> Response {
    match metric_snapshot(&st.db).await {
        Ok(view) => match render_metrics(&st.db, &view).await {
            Ok(body) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, METRICS_CONTENT_TYPE)],
                body,
            )
                .into_response(),
            Err(error) => metrics_error(error),
        },
        Err(error) => metrics_error(error),
    }
}

fn metrics_error(error: anyhow::Error) -> Response {
    tracing::error!(error = %format!("{error:?}"), "metrics snapshot failed");
    (StatusCode::SERVICE_UNAVAILABLE, "metrics unavailable\n").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_label_values_are_escaped() {
        assert_eq!(label_value("a\\b\n\"c"), "a\\\\b\\n\\\"c");
    }

    #[test]
    fn arbitrary_dimensions_collapse_to_other() {
        assert_eq!(bounded_status("branch-name"), "other");
        assert_eq!(bounded_class("user-name"), "other");
        assert_eq!(bounded_protocol("token-id"), "other");
        assert_eq!(bounded_source("repository-path"), "other");
    }
}
