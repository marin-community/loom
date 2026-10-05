//! One occurrence runtime dispatches scheduled scripts and agents.
use super::{ApiResult, AppError, AppState};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;
use weaver_api::{SlackDeliveryStatus, SlackDeliveryView};
use weaver_core::{
    occurrence::{self, Occurrence, OccurrenceStatus},
    watch::Watch,
};

#[derive(sqlx::FromRow)]
struct Delivery {
    channel: String,
    text: String,
    status: SlackDeliveryStatus,
    slack_ts: Option<String>,
    error: Option<String>,
    retry_at: Option<String>,
}

pub(crate) async fn run(state: AppState) {
    let mut tasks = tokio::task::JoinSet::new();
    let mut processing = HashMap::new();
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {
            Some(result) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                match result {
                    Ok((task_id, result)) => {
                        let occurrence = processing.remove(&task_id);
                        if let Err(error) = result { tracing::warn!(%error, ?occurrence, "scheduled work failed"); }
                    }
                    Err(error) => {
                        processing.remove(&error.id());
                        tracing::error!(%error, "scheduled task stopped");
                    }
                }
            }
            _ = timer.tick() => {
                let enabled = weaver_core::config::get_bool(&state.db, "watch.enabled", weaver_core::config::DEFAULT_WATCH_ENABLED).await;
                if enabled {
                    if let Err(error) = occurrence::tick(&state.db, Utc::now()).await { tracing::warn!(%error, "scheduled timer failed"); }
                }
                let active = match occurrence::active(&state.db).await {
                    Ok(active) => active,
                    Err(error) => { tracing::warn!(%error, "scheduled recovery failed"); continue; }
                };
                for occurrence in active {
                    if processing.values().any(|id| id == &occurrence.id) { continue; }
                    let id = occurrence.id.clone();
                    let state = state.clone();
                    let task = tasks.spawn(async move {
                        let watch: Watch = serde_json::from_str(&occurrence.definition)?;
                        if watch.agent()?.is_none() {
                            if enabled { crate::watch::execute_script_occurrence(&state, &occurrence).await?; }
                            else { crate::watch::recover_script_occurrence(&state, &occurrence).await?; }
                            return Ok(());
                        }
                        if monitor(&state, &occurrence).await? { return Ok(()); }
                        if matches!(occurrence.status, OccurrenceStatus::Pending | OccurrenceStatus::Dispatching) && enabled { launch(&state, &occurrence).await?; }
                        Ok::<_, anyhow::Error>(())
                    });
                    processing.insert(task.id(), id);
                }
            }
        }
    }
}

async fn launch(state: &AppState, occurrence: &Occurrence) -> Result<()> {
    let watch: Watch = serde_json::from_str(&occurrence.definition)?;
    let agent = watch
        .agent()?
        .ok_or_else(|| anyhow::anyhow!("missing agent target"))?;
    let subject = format!("watch:{}", watch.id);
    let profiles = vec![agent.profile.clone()];
    let principal = crate::auth::Principal {
        username: subject.clone(),
        github_login: None,
        via: crate::auth::AuthVia::Loopback,
        grant: crate::auth::Grant::Automation {
            subject: subject.clone(),
            profiles: profiles.clone(),
        },
        automation_context: None,
    };
    let prompt = format!("{}\n\nScheduled watch context: occurrence={}, scheduled_at={}, deadline_at={}, trigger={}. Complete this task using as many tool calls as needed, then report your result without waiting for another message. Persistent memory is available through `loom watch state`; use its version for writes. Allowed Slack channels: {}. Use `loom branches slack post` with a stable action key when sending a message.\n", agent.prompt, occurrence.id, occurrence.scheduled_at, occurrence.deadline_at, occurrence.trigger_reason, agent.slack_channels.join(", "));
    let request = weaver_api::operations::runs::create::Input {
        profile: agent.profile,
        idempotency_key: occurrence.id.clone(),
        source: "watch".into(),
        watch_id: Some(watch.id.clone()),
        channel: None,
        slack: None,
        session: serde_json::from_value(
            json!({"repo": agent.repo, "goal": prompt, "title": format!("Watch {}", watch.name)}),
        )?,
    };
    let launch =
        super::automation::create_run_core(state, &principal, request, subject, profiles, true);
    tokio::pin!(launch);
    let remaining = (chrono::DateTime::parse_from_rfc3339(&occurrence.deadline_at)?
        .with_timezone(&Utc)
        - Utc::now())
    .to_std()
    .unwrap_or_default();
    let mut checks = tokio::time::interval(std::time::Duration::from_secs(1));
    let timeout = tokio::time::sleep(remaining);
    tokio::pin!(timeout);
    let result = loop {
        tokio::select! {
            result = &mut launch => break result,
            _ = &mut timeout => {
                let current = load_current(state, &occurrence.id).await?;
                settle(state, &current, OccurrenceStatus::Error, "Agent exceeded the occurrence deadline").await?;
                stop(state, &current).await?;
                // Provisioning may finish late. Its cancelled reservation prevents
                // promotion; retain the overlap gate until that future drains.
                if let Err(error) = launch.await {
                    tracing::warn!(error = %error.message, occurrence = %occurrence.id, "provisioning failed after occurrence deadline");
                }
                let current = load_current(state, &occurrence.id).await?;
                close(state, &current, OccurrenceStatus::Error, "Agent exceeded the occurrence deadline").await?;
                return Ok(());
            }
            _ = checks.tick() => {
                let occurrence = load_current(state, &occurrence.id).await?;
                if pending_invalid(state, &occurrence).await? {
                    settle(state, &occurrence, OccurrenceStatus::Cancelled, "Definition changed during provisioning").await?;
                    stop(state, &occurrence).await?;
                    if let Err(error) = launch.await {
                        tracing::warn!(error = %error.message, occurrence = %occurrence.id, "provisioning failed after occurrence cancellation");
                    }
                    close(state, &load_current(state, &occurrence.id).await?, OccurrenceStatus::Cancelled, "Definition changed during provisioning").await?;
                    return Ok(());
                }
            }
        }
    };
    match result {
        Ok(run) => {
            sqlx::query("UPDATE watch_occurrences SET run_id = ?, session_id = ? WHERE id = ?")
                .bind(&run.id)
                .bind(&run.session_id)
                .bind(&occurrence.id)
                .execute(&state.db)
                .await?;
            if run.status == "running" {
                occurrence::started(&state.db, occurrence, &run.id, &run.session_id).await?;
            } else if matches!(run.status.as_str(), "failed" | "cancelled" | "completed") {
                close(
                    state,
                    &load_current(state, &occurrence.id).await?,
                    OccurrenceStatus::Error,
                    &run.summary,
                )
                .await?;
            }
        }
        Err(error) => {
            // A final launch failure may leave a stopped, recoverable session.
            // Find the reservation so teardown precedes releasing ownership.
            if let Some(run) =
                crate::runs::get_by_key(&state.db, &format!("watch:{}", watch.id), &occurrence.id)
                    .await?
            {
                sqlx::query("UPDATE watch_occurrences SET run_id = ?, session_id = ?, status = 'running' WHERE id = ?")
                    .bind(&run.id).bind(&run.session_id).bind(&occurrence.id).execute(&state.db).await?;
                close(
                    state,
                    &load_current(state, &occurrence.id).await?,
                    OccurrenceStatus::Error,
                    &error.message,
                )
                .await?;
            } else {
                occurrence::finish(
                    &state.db,
                    occurrence,
                    OccurrenceStatus::Error,
                    &error.message,
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Returns true when the occurrence has closed or is being torn down.
async fn monitor(state: &AppState, occurrence: &Occurrence) -> Result<bool> {
    if occurrence.status == OccurrenceStatus::Finishing {
        close(
            state,
            occurrence,
            occurrence
                .settlement_outcome
                .unwrap_or(OccurrenceStatus::Error),
            occurrence
                .settlement_summary
                .as_deref()
                .unwrap_or("Interrupted teardown"),
        )
        .await?;
        return Ok(true);
    }
    let cancelled = pending_invalid(state, occurrence).await?;
    let expired = chrono::DateTime::parse_from_rfc3339(&occurrence.deadline_at)? <= Utc::now();
    let session = match occurrence.session_id.as_deref() {
        Some(id) => crate::session::get(&state.db, id).await?,
        None => None,
    };
    let completed = session
        .as_ref()
        .is_some_and(|s| s.turn_count > 0 && s.acp_inflight.is_none());
    let terminal = session
        .as_ref()
        .is_some_and(|s| matches!(s.status.as_str(), "done" | "error" | "archived"));
    if !cancelled && !expired && !completed && !terminal {
        return Ok(false);
    }
    let (outcome, summary) = if cancelled {
        (OccurrenceStatus::Cancelled, "Watch paused or removed")
    } else if expired {
        (
            OccurrenceStatus::Error,
            "Agent exceeded the occurrence deadline",
        )
    } else if completed {
        let reason =
            crate::chat::latest_stop_reason(&state.db, &session.as_ref().unwrap().id).await?;
        if reason.as_deref() == Some("end_turn") {
            (OccurrenceStatus::Ok, "Agent completed")
        } else {
            (OccurrenceStatus::Error, "Agent stopped without completing")
        }
    } else {
        (OccurrenceStatus::Error, "Agent session stopped")
    };
    close(state, occurrence, outcome, summary).await?;
    Ok(true)
}

async fn pending_invalid(state: &AppState, occurrence: &Occurrence) -> Result<bool> {
    if !matches!(
        occurrence.status,
        OccurrenceStatus::Pending | OccurrenceStatus::Dispatching
    ) {
        return Ok(false);
    }
    if let Some(id) = occurrence.session_id.as_deref() {
        if crate::session::get(&state.db, id)
            .await?
            .is_some_and(|session| session.turn_count > 0 || session.acp_inflight.is_some())
        {
            return Ok(false);
        }
    }
    let watch = weaver_core::watch::get(&state.db, &occurrence.watch_id).await?;
    Ok(watch.as_ref().is_none_or(|watch| {
        watch.revision != occurrence.revision
            || ((!watch.enabled || watch.paused) && occurrence.automatic)
    }))
}

async fn load_current(state: &AppState, id: &str) -> Result<Occurrence> {
    Ok(
        sqlx::query_as("SELECT * FROM watch_occurrences WHERE id = ?")
            .bind(id)
            .fetch_one(&state.db)
            .await?,
    )
}
async fn settle(
    state: &AppState,
    occurrence: &Occurrence,
    outcome: OccurrenceStatus,
    summary: &str,
) -> Result<()> {
    sqlx::query("UPDATE watch_occurrences SET status = 'finishing', settlement_outcome = COALESCE(settlement_outcome, ?), settlement_summary = COALESCE(settlement_summary, ?) WHERE id = ?")
        .bind(outcome).bind(summary).bind(&occurrence.id).execute(&state.db).await?;
    Ok(())
}
async fn stop(state: &AppState, occurrence: &Occurrence) -> Result<()> {
    if let Some(id) = occurrence.session_id.as_deref() {
        crate::runs::cancel_for_session_with_summary(
            &state.db,
            id,
            "Scheduled occurrence finishing",
        )
        .await?;
        stop_session(state, id).await?;
    }
    Ok(())
}

async fn stop_session(state: &AppState, id: &str) -> Result<()> {
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((session, branch)) = crate::session::with_branch(&state.db, id).await? else {
        return crate::backend::kill_session_and_wait(&format!("weaver-{id}")).await;
    };
    let session = if session.lifecycle_transition.as_deref() == Some("finishing") {
        crate::lifecycle::release_abandoned_transition(&state.db, &session).await?
    } else {
        session
    };
    crate::lifecycle::require_no_transition(&session)?;
    if session.status == "archived" {
        return crate::backend::kill_session_and_wait(&session.term_session).await;
    }
    if !crate::session::begin_transition(&state.db, id, "finishing", "Stopping scheduled agent")
        .await?
    {
        anyhow::bail!("another lifecycle operation owns this session");
    }
    let result = async {
        // A different daemon may have archived the row before our claim.
        let session = crate::session::get(&state.db, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("scheduled session disappeared"))?;
        crate::backend::kill_session_and_wait(&session.term_session).await?;
        crate::auth::revoke_session_tokens(&state.db, id).await?;
        crate::session::set_inflight(&state.db, id, None).await?;
        let status = if session.status == "archived" {
            "archived"
        } else {
            "done"
        };
        if status == "done" {
            crate::session::touch(&state.db, id).await?;
        }
        if !crate::session::complete_transition(&state.db, id, "finishing", status).await? {
            anyhow::bail!("scheduled completion lost its lifecycle transition");
        }
        weaver_core::events::record(
            &state.db,
            &state.bus,
            &branch.id,
            "status",
            json!({"status": status, "reason": "Scheduled occurrence finished"}),
        )
        .await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        if let Err(error) = crate::session::clear_transition(&state.db, id, "finishing").await {
            tracing::warn!(%error, session = id, "could not release failed scheduled completion");
        }
    }
    result
}

async fn close(
    state: &AppState,
    occurrence: &Occurrence,
    outcome: OccurrenceStatus,
    summary: &str,
) -> Result<()> {
    settle(state, occurrence, outcome, summary).await?;
    stop(state, occurrence).await?;
    let current = load_current(state, &occurrence.id).await?;
    let outcome = current.settlement_outcome.unwrap_or(outcome);
    let summary = current.settlement_summary.as_deref().unwrap_or(summary);
    if let Some(run) = &current.run_id {
        sqlx::query("UPDATE automation_runs SET status = 'completed', outcome = ?, summary = ?, updated_at = ? WHERE id = ?")
            .bind(outcome).bind(summary).bind(weaver_core::db::now_iso()).bind(run).execute(&state.db).await?;
    }
    occurrence::finish(&state.db, &current, outcome, summary).await
}

pub(super) async fn own_occurrence(state: &AppState, branch: &str) -> ApiResult<Occurrence> {
    let session = crate::session::active_for_branch(&state.db, branch)
        .await?
        .ok_or_else(|| AppError::bad_request("no active scheduled agent on this branch"))?;
    occurrence::for_session(&state.db, &session.id)
        .await?
        .ok_or_else(|| {
            AppError::bad_request("this session does not own an active watch occurrence")
        })
}

pub(super) async fn slack_post(
    state: &AppState,
    input: weaver_api::operations::branches::slack::post::Input,
) -> ApiResult<SlackDeliveryView> {
    let occurrence = own_occurrence(state, &input.branch).await?;
    let watch: Watch = serde_json::from_str(&occurrence.definition)?;
    let agent = watch
        .agent()?
        .ok_or_else(|| AppError::bad_request("missing agent target"))?;
    if !agent.slack_channels.contains(&input.channel) {
        return Err(AppError::bad_request(
            "Slack channel is outside this occurrence's destinations",
        ));
    }
    if input.text.trim().is_empty()
        || input.text.len() > 4000
        || input.action_key.is_empty()
        || input.action_key.len() > 128
    {
        return Err(AppError::bad_request(
            "text must be 1..4000 bytes and action_key 1..128 bytes",
        ));
    }
    let web = crate::slack::SlackWeb::from_db(&state.db)
        .await
        .ok_or_else(|| AppError::bad_request("Slack is not configured"))?;
    let inserted = sqlx::query("INSERT OR IGNORE INTO watch_deliveries (occurrence_id,action_key,channel,text,status) VALUES (?,?,?,?,'uncertain')")
        .bind(&occurrence.id).bind(&input.action_key).bind(&input.channel).bind(&input.text).execute(&state.db).await?.rows_affected();
    let mut send = inserted > 0;
    if inserted == 0 {
        let delivery: Delivery = sqlx::query_as("SELECT channel,text,status,slack_ts,error,retry_at FROM watch_deliveries WHERE occurrence_id = ? AND action_key = ?").bind(&occurrence.id).bind(&input.action_key).fetch_one(&state.db).await?;
        if delivery.channel != input.channel || delivery.text != input.text {
            return Err(AppError::conflict(
                "action_key already belongs to another message",
            ));
        }
        if delivery.status == SlackDeliveryStatus::Rejected {
            send = sqlx::query("UPDATE watch_deliveries SET status = 'uncertain' WHERE occurrence_id = ? AND action_key = ? AND status = 'rejected' AND retry_at <= ?")
                .bind(&occurrence.id).bind(&input.action_key).bind(weaver_core::db::now_iso()).execute(&state.db).await?.rows_affected() > 0;
        }
        if !send {
            return Ok(SlackDeliveryView {
                status: delivery.status,
                posted: delivery.status == SlackDeliveryStatus::Posted,
                ts: delivery.slack_ts,
                error: delivery.error,
                retry_at: delivery.retry_at,
            });
        }
    }
    match web.post_message(&input.channel, None, &input.text).await {
        Ok(ts) => {
            sqlx::query("UPDATE watch_deliveries SET status = 'posted', slack_ts = ? WHERE occurrence_id = ? AND action_key = ?").bind(&ts).bind(&occurrence.id).bind(&input.action_key).execute(&state.db).await?;
            Ok(SlackDeliveryView {
                posted: true,
                status: SlackDeliveryStatus::Posted,
                ts: Some(ts),
                error: None,
                retry_at: None,
            })
        }
        Err(error) => {
            if let Some(rejection) = error.downcast_ref::<crate::slack::SlackRejection>() {
                let delay = match rejection {
                    crate::slack::SlackRejection::RateLimited { retry_secs } => {
                        i64::try_from(*retry_secs).ok()
                    }
                    _ => Some(0),
                };
                let retry_at = delay
                    .and_then(chrono::Duration::try_seconds)
                    .and_then(|delay| Utc::now().checked_add_signed(delay))
                    .map(weaver_core::schedule::iso);
                sqlx::query("UPDATE watch_deliveries SET status = 'rejected', error = ?, retry_at = ? WHERE occurrence_id = ? AND action_key = ?")
                    .bind(error.to_string()).bind(&retry_at).bind(&occurrence.id).bind(&input.action_key).execute(&state.db).await?;
                return Ok(SlackDeliveryView {
                    posted: false,
                    status: SlackDeliveryStatus::Rejected,
                    ts: None,
                    error: Some(error.to_string()),
                    retry_at,
                });
            }
            sqlx::query(
                "UPDATE watch_deliveries SET error = ? WHERE occurrence_id = ? AND action_key = ?",
            )
            .bind(error.to_string())
            .bind(&occurrence.id)
            .bind(&input.action_key)
            .execute(&state.db)
            .await?;
            Err(AppError::bad_request(format!(
                "Slack delivery is uncertain; reuse the action key to inspect it: {error}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (AppState, Watch, Occurrence) {
        let db = weaver_core::db::connect_in_memory().await.unwrap();
        let state = AppState {
            trigger: crate::github_trigger::GithubTrigger::production(db.clone()),
            ctx: crate::Ctx {
                db,
                bus: weaver_core::events::EventBus::new(),
                addr: "127.0.0.1:0".into(),
            },
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(std::path::PathBuf::from(
                "/tmp/unused-scheduled-test-editor",
            ))),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        };
        let watch = weaver_core::watch::create(
            &state.db,
            &weaver_core::watch::NewWatch {
                name: "scheduled".into(),
                enabled: true,
                agent: Some(weaver_core::schedule::AgentTarget {
                    profile: "watch".into(),
                    repo: "org/repo".into(),
                    prompt: "Check jobs".into(),
                    slack_channels: vec![],
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        occurrence::enqueue(
            &state.db,
            &watch,
            &occurrence::EnqueueRequest {
                due: Utc::now(),
                reason: "event",
                automatic: true,
                dry_run: false,
                trigger_context: json!({"event":"event"}),
            },
            Utc::now(),
        )
        .await
        .unwrap();
        let occurrence = occurrence::active(&state.db).await.unwrap().remove(0);
        (state, watch, occurrence)
    }
    #[tokio::test]
    async fn expired_unlaunched_occurrence_releases_gate_with_error() {
        let (state, _, mut occurrence) = fixture().await;
        occurrence.deadline_at =
            weaver_core::schedule::iso(Utc::now() - chrono::Duration::seconds(1));
        assert!(monitor(&state, &occurrence).await.unwrap());
        assert!(occurrence::active(&state.db).await.unwrap().is_empty());
        assert_eq!(
            load_current(&state, &occurrence.id)
                .await
                .unwrap()
                .settlement_outcome,
            Some(OccurrenceStatus::Error)
        );
    }
    #[tokio::test]
    async fn restart_preserves_intended_result_during_settlement() {
        let (state, _, occurrence) = fixture().await;
        settle(
            &state,
            &occurrence,
            OccurrenceStatus::Ok,
            "Job check complete",
        )
        .await
        .unwrap();
        let occurrence = load_current(&state, &occurrence.id).await.unwrap();
        assert!(monitor(&state, &occurrence).await.unwrap());
        let result = weaver_core::watch::recent_runs(&state.db, &occurrence.watch_id, 1)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(result.outcome, "ok");
        assert_eq!(result.summary, "Job check complete");
    }
    #[tokio::test]
    async fn pause_cancels_pending_work_but_preserves_active_turn() {
        let (state, watch, occurrence) = fixture().await;
        sqlx::query("UPDATE watches SET enabled = 0, paused = 1 WHERE id = ?")
            .bind(&watch.id)
            .execute(&state.db)
            .await
            .unwrap();
        let mut running = occurrence.clone();
        running.status = OccurrenceStatus::Running;
        assert!(!monitor(&state, &running).await.unwrap());
        assert!(monitor(&state, &occurrence).await.unwrap());
        assert_eq!(
            load_current(&state, &occurrence.id).await.unwrap().status,
            OccurrenceStatus::Cancelled
        );
    }
    #[tokio::test]
    async fn external_automation_cannot_use_scheduler_source() {
        let (state, _, _) = fixture().await;
        let principal = crate::auth::Principal::anonymous();
        let input = serde_json::from_value(
            json!({"profile":"watch", "source":"watch", "idempotency_key":"forged", "session":{}}),
        )
        .unwrap();
        let error = super::super::automation::create_run_core(
            &state,
            &principal,
            input,
            "external".into(),
            vec!["watch".into()],
            false,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    }
}
