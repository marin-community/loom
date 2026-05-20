//! Background task: periodically runs a headless agent to summarize each
//! workspace's diff against its merge base.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::web::AppState;
use crate::workspace::Workspace;
use crate::{agent, config, events, git, workspace};

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
        let workspaces = workspace::list(&state.db).await.unwrap_or_default();
        let due: Vec<Workspace> = workspaces
            .into_iter()
            .filter(|ws| !workspace::is_terminal(&ws.status) && is_due(ws, interval))
            .collect();
        tracing::debug!(interval_secs = interval, due = due.len(), "summary tick");
        for ws in due {
            match summarize_workspace(&state, &ws).await {
                Ok(_) => tracing::info!(id = %ws.id, "summarized workspace"),
                Err(e) => {
                    tracing::warn!("summary for {} failed: {e}", ws.id);
                    // Mark it summarized anyway so a broken workspace is not retried
                    // every single minute.
                    let _ = workspace::mark_summarized(&state.db, &ws.id).await;
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

/// A workspace is due for a summary once the interval has elapsed since the
/// last one *and* there has been activity since then.
fn is_due(ws: &Workspace, interval: i64) -> bool {
    let Some(last) = &ws.summary_updated_at else {
        return true;
    };
    let last_dt = parse(last);
    let elapsed_ok = match last_dt {
        Some(d) => (Utc::now() - d).num_seconds() >= interval,
        None => true,
    };
    let has_activity = match (parse(&ws.last_activity_at), last_dt) {
        (Some(activity), Some(summarized)) => activity > summarized,
        _ => true,
    };
    elapsed_ok && has_activity
}

/// Summarize one workspace now. Returns the new description.
pub async fn summarize_workspace(state: &AppState, ws: &Workspace) -> Result<String> {
    let work_dir = PathBuf::from(&ws.work_dir);
    let base = git::merge_base(&work_dir, &ws.base_branch).await?;
    let patch = git::diff(&work_dir, &base).await?;
    if patch.trim().is_empty() {
        tracing::debug!(id = %ws.id, "no diff to summarize");
        workspace::mark_summarized(&state.db, &ws.id).await?;
        return Ok(ws.description.clone());
    }
    let stat = git::diff_stat(&work_dir, &base).await?;
    let command = config::get_or(
        &state.db,
        "agent.summary_command",
        config::DEFAULT_SUMMARY_COMMAND,
    )
    .await;
    tracing::debug!(
        id = %ws.id,
        patch_len = patch.len(),
        files_changed = stat.files_changed,
        %command,
        "summarizing workspace diff"
    );
    let description = agent::summarize(&work_dir, &command, &patch).await?;

    sqlx::query(
        "INSERT INTO summaries (workspace_id, description, files_changed, insertions, deletions)
         VALUES (?,?,?,?,?)",
    )
    .bind(&ws.id)
    .bind(&description)
    .bind(stat.files_changed)
    .bind(stat.insertions)
    .bind(stat.deletions)
    .execute(&state.db)
    .await?;
    workspace::set_summary(&state.db, &ws.id, &description).await?;
    events::record(
        &state.db,
        &state.bus,
        &ws.id,
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
