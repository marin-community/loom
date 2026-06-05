//! Background task: periodically runs a headless agent to summarize each
//! session's diff against its merge base.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::session::{self as session_mod, Session};
use crate::web::AppState;
use crate::{agent, config, events, git};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;

pub async fn run(state: AppState) {
    tracing::info!("summary loop started");
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let interval = config::get_i64(
            &state.db,
            "summary.interval_secs",
            config::DEFAULT_SUMMARY_INTERVAL_SECS,
        )
        .await;
        let sessions = session_mod::list(&state.db).await.unwrap_or_default();
        let due: Vec<Session> = sessions
            .into_iter()
            .filter(|s| !session_mod::is_terminal(&s.status) && is_due(s, interval))
            .collect();
        for session in due {
            let Ok(Some(branch)) = branch_mod::get(&state.db, &session.branch_id).await else {
                continue;
            };
            match summarize_session(&state, &session, &branch).await {
                Ok(_) => tracing::info!(id = %session.id, "summarized session"),
                Err(e) => {
                    tracing::warn!("summary for {} failed: {e}", session.id);
                    let _ = session_mod::mark_summarized(&state.db, &session.id).await;
                }
            }
        }
    }
}

fn parse(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn is_due(session: &Session, interval: i64) -> bool {
    let Some(last) = &session.summary_updated_at else {
        return true;
    };
    let last_dt = parse(last);
    let elapsed_ok = match last_dt {
        Some(d) => (Utc::now() - d).num_seconds() >= interval,
        None => true,
    };
    let has_activity = match (session.last_activity_at.as_deref().and_then(parse), last_dt) {
        (Some(activity), Some(summarized)) => activity > summarized,
        _ => true,
    };
    elapsed_ok && has_activity
}

/// Summarize one session now. Returns the new description.
pub async fn summarize_session(
    state: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<String> {
    let work_dir = PathBuf::from(&session.work_dir);
    let base = git::merge_base(&work_dir, &branch.base_branch).await?;
    let patch = git::diff(&work_dir, &base).await?;
    if patch.trim().is_empty() {
        tracing::debug!(id = %session.id, "no diff to summarize");
        session_mod::mark_summarized(&state.db, &session.id).await?;
        return Ok(branch.description.clone());
    }
    let stat = git::diff_stat(&work_dir, &base).await?;
    let command = config::get_or(
        &state.db,
        "agent.summary_command",
        config::DEFAULT_SUMMARY_COMMAND,
    )
    .await;
    let description = agent::summarize(&work_dir, &command, &patch).await?;

    sqlx::query(
        "INSERT INTO summaries (session_id, description, files_changed, insertions, deletions)
         VALUES (?,?,?,?,?)",
    )
    .bind(&session.id)
    .bind(&description)
    .bind(stat.files_changed)
    .bind(stat.insertions)
    .bind(stat.deletions)
    .execute(&state.db)
    .await?;
    branch_mod::set_description(&state.db, &branch.id, &description).await?;
    session_mod::mark_summarized(&state.db, &session.id).await?;
    events::record(
        &state.db,
        &state.bus,
        &branch.id,
        "summary",
        json!({
            "description": description,
            "files_changed": stat.files_changed,
            "insertions": stat.insertions,
            "deletions": stat.deletions,
        }),
    )
    .await?;
    Ok(description)
}
