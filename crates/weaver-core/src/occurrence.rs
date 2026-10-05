//! Durable scheduling and ownership. Runtime launch and teardown remain in Loom.
use crate::{
    db::{now_iso, Db},
    schedule,
    watch::{self, Watch},
};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum OccurrenceStatus {
    Pending,
    Dispatching,
    Running,
    Finishing,
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Occurrence {
    pub id: String,
    pub watch_id: String,
    pub revision: i64,
    pub scheduled_at: String,
    pub trigger_reason: String,
    pub definition: String,
    pub watch_run_id: i64,
    pub status: OccurrenceStatus,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub queued_at: String,
    pub deadline_at: String,
    pub settlement_outcome: Option<OccurrenceStatus>,
    pub settlement_summary: Option<String>,
}

pub async fn active(db: &Db) -> Result<Vec<Occurrence>> {
    Ok(sqlx::query_as("SELECT * FROM watch_occurrences WHERE status IN ('pending','dispatching','running','finishing') ORDER BY queued_at").fetch_all(db).await?)
}
pub async fn for_session(db: &Db, session: &str) -> Result<Option<Occurrence>> {
    Ok(sqlx::query_as(
        "SELECT * FROM watch_occurrences WHERE session_id = ? AND status IN ('dispatching','running')",
    )
    .bind(session)
    .fetch_optional(db)
    .await?)
}
pub async fn history(db: &Db, watch: &str, limit: i64) -> Result<Vec<Occurrence>> {
    Ok(sqlx::query_as(
        "SELECT * FROM watch_occurrences WHERE watch_id = ? ORDER BY queued_at DESC LIMIT ?",
    )
    .bind(watch)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

/// Claim one occurrence and advance its cadence in one write transaction. The
/// definition revision prevents a timer holding an old snapshot from firing it.
pub async fn enqueue(
    db: &Db,
    snapshot: &Watch,
    due: DateTime<Utc>,
    reason: &str,
    automatic: bool,
    now: DateTime<Utc>,
) -> Result<Option<i64>> {
    let mut tx = db.begin().await?;
    let claimed = sqlx::query("UPDATE watches SET last_run_at = last_run_at WHERE id = ? AND revision = ? AND (? = 0 OR (enabled = 1 AND paused = 0))")
        .bind(&snapshot.id).bind(snapshot.revision).bind(automatic).execute(&mut *tx).await?.rows_affected();
    if claimed == 0 {
        return Ok(None);
    }
    let watch: Watch = sqlx::query_as("SELECT * FROM watches WHERE id = ?")
        .bind(&snapshot.id)
        .fetch_one(&mut *tx)
        .await?;
    let scheduled = reason == "schedule";
    if scheduled && watch.next_run_at.as_deref() != Some(schedule::iso(due).as_str()) {
        return Ok(None);
    }
    let next = if scheduled {
        schedule::next(
            &watch.trigger(),
            schedule::latest_due(&watch.trigger(), due, now)?,
        )?
        .map(schedule::iso)
    } else {
        watch.next_run_at.clone()
    };
    let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM watch_occurrences WHERE watch_id = ? AND status IN ('pending','dispatching','running','finishing'))").bind(&watch.id).fetch_one(&mut *tx).await?;
    let selected = if scheduled {
        schedule::latest_due(&watch.trigger(), due, now)?
    } else {
        due
    };
    let late =
        scheduled && now.signed_duration_since(selected).num_seconds() > schedule::LATE_GRACE_SECS;
    let run_id: i64 = sqlx::query_scalar("INSERT INTO watch_runs (watch_id,trigger_reason,trigger_event,started_at,outcome,summary,finished_at) VALUES (?,?,?,?,?,?,?) RETURNING id")
        .bind(&watch.id).bind(reason).bind(reason).bind(schedule::iso(now))
        .bind(if busy || late {"skipped"} else {"queued"})
        .bind(if busy {"Previous agent occurrence is still active"} else if late {"Missed occurrence exceeds late grace"} else {"Queued agent occurrence"})
        .bind((busy || late).then(|| schedule::iso(now))).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE watches SET next_run_at = ?, last_run_at = ? WHERE id = ?")
        .bind(next)
        .bind(schedule::iso(now))
        .bind(&watch.id)
        .execute(&mut *tx)
        .await?;
    if !busy && !late {
        sqlx::query("INSERT INTO watch_occurrences (id,watch_id,revision,scheduled_at,trigger_reason,definition,watch_run_id,status,queued_at,deadline_at) VALUES (?,?,?,?,?,?,?,'pending',?,?)")
            .bind(crate::branch::new_id()).bind(&watch.id).bind(watch.revision).bind(schedule::iso(selected)).bind(reason).bind(serde_json::to_string(&watch)?).bind(run_id).bind(schedule::iso(now))
            .bind(schedule::iso(now + Duration::seconds(watch.run_timeout_secs))).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Some(run_id))
}

pub async fn tick(db: &Db, now: DateTime<Utc>) -> Result<()> {
    for watch in watch::list_enabled(db).await? {
        if watch.agent_spec.is_none() || watch.paused || !watch.trigger().is_scheduled() {
            continue;
        }
        match watch.next_run_at.as_deref() {
            Some(time) => {
                let due = DateTime::parse_from_rfc3339(time)?.with_timezone(&Utc);
                if due <= now {
                    enqueue(db, &watch, due, "schedule", true, now).await?;
                }
            }
            None => {
                let next = schedule::next(&watch.trigger(), now)?.map(schedule::iso);
                sqlx::query("UPDATE watches SET next_run_at = ? WHERE id = ? AND revision = ? AND next_run_at IS NULL AND enabled = 1 AND paused = 0").bind(next).bind(&watch.id).bind(watch.revision).execute(db).await?;
            }
        }
    }
    Ok(())
}

pub async fn started(db: &Db, occurrence: &Occurrence, run: &str, session: &str) -> Result<()> {
    sqlx::query("UPDATE watch_occurrences SET run_id = ?, session_id = ?, status = 'running' WHERE id = ? AND status IN ('pending','dispatching')")
        .bind(run).bind(session).bind(&occurrence.id).execute(db).await?;
    sqlx::query(
        "UPDATE watch_runs SET outcome = 'running', summary = 'Agent running' WHERE id = ?",
    )
    .bind(occurrence.watch_run_id)
    .execute(db)
    .await?;
    Ok(())
}
pub async fn finish(
    db: &Db,
    occurrence: &Occurrence,
    outcome: OccurrenceStatus,
    summary: &str,
) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE watch_occurrences SET status = ? WHERE id = ?")
        .bind(outcome)
        .bind(&occurrence.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE watch_runs SET outcome = ?, summary = ?, finished_at = ? WHERE id = ?")
        .bind(outcome)
        .bind(summary)
        .bind(now_iso())
        .bind(occurrence.watch_run_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn state(
    db: &Db,
    occurrence: &Occurrence,
    value: &serde_json::Value,
    version: i64,
) -> Result<i64> {
    if !value.is_object() {
        return Err(anyhow!("watch state must be a JSON object"));
    }
    if value.to_string().len() > 65536 {
        return Err(anyhow!("state exceeds 64 KiB"));
    }
    let changed = sqlx::query("UPDATE watches SET state = ?, state_version = state_version + 1 WHERE id = ? AND state_version = ? AND EXISTS(SELECT 1 FROM watch_occurrences WHERE id = ? AND status IN ('dispatching','running'))")
        .bind(value.to_string()).bind(&occurrence.watch_id).bind(version).bind(&occurrence.id).execute(db).await?.rows_affected();
    if changed == 0 {
        return Err(anyhow!(
            "state version changed or occurrence is no longer active"
        ));
    }
    Ok(version + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::connect_in_memory,
        watch::{NewWatch, WatchUpdate},
    };
    async fn fixture() -> (Db, Watch, DateTime<Utc>) {
        let db = connect_in_memory().await.unwrap();
        let watch = watch::create(
            &db,
            &NewWatch {
                name: "hourly".into(),
                enabled: true,
                trigger_spec: r#"{"every":"5m"}"#.into(),
                agent: Some(schedule::AgentTarget {
                    profile: "watch".into(),
                    repo: "org/repo".into(),
                    prompt: "check jobs".into(),
                    slack_channels: vec![],
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-05T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        (db, watch, now)
    }
    #[tokio::test]
    async fn overdue_cadence_is_atomic_and_does_not_drift() {
        let (db, watch, now) = fixture().await;
        tick(&db, now).await.unwrap();
        tick(&db, now + Duration::minutes(17)).await.unwrap();
        let occurrences = active(&db).await.unwrap();
        assert_eq!(occurrences.len(), 1);
        assert_eq!(
            watch::get(&db, &watch.id)
                .await
                .unwrap()
                .unwrap()
                .next_run_at
                .as_deref(),
            Some("2026-10-05T10:20:00.000Z")
        );
        tick(&db, now + Duration::minutes(17)).await.unwrap();
        assert_eq!(history(&db, &watch.id, 10).await.unwrap().len(), 1);
        enqueue(
            &db,
            &watch,
            now + Duration::minutes(18),
            "manual",
            false,
            now,
        )
        .await
        .unwrap();
        assert_eq!(active(&db).await.unwrap().len(), 1);
        assert_eq!(
            watch::recent_runs(&db, &watch.id, 1).await.unwrap()[0].outcome,
            "skipped"
        );
    }
    #[tokio::test]
    async fn stale_revision_and_disabled_timer_cannot_enqueue() {
        let (db, watch, now) = fixture().await;
        watch::update(
            &db,
            &watch.id,
            &WatchUpdate {
                agent_spec: Some(watch.agent_spec.clone().unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(enqueue(&db, &watch, now, "manual", false, now)
            .await
            .unwrap()
            .is_none());
        let watch = watch::get(&db, &watch.id).await.unwrap().unwrap();
        watch::set_enabled(&db, &watch.id, false).await.unwrap();
        assert!(enqueue(&db, &watch, now, "event", true, now)
            .await
            .unwrap()
            .is_none());
        assert!(active(&db).await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn state_requires_live_owner_and_current_version() {
        let (db, watch, now) = fixture().await;
        enqueue(&db, &watch, now, "manual", false, now)
            .await
            .unwrap();
        let occurrence = active(&db).await.unwrap().remove(0);
        assert!(state(&db, &occurrence, &serde_json::json!({}), 0)
            .await
            .is_err());
        started(&db, &occurrence, "run", "session").await.unwrap();
        assert_eq!(
            state(&db, &occurrence, &serde_json::json!({"seen":1}), 0)
                .await
                .unwrap(),
            1
        );
        assert!(state(&db, &occurrence, &serde_json::json!({}), 0)
            .await
            .is_err());
        finish(&db, &occurrence, OccurrenceStatus::Ok, "done")
            .await
            .unwrap();
        assert!(state(&db, &occurrence, &serde_json::json!({}), 1)
            .await
            .is_err());
        assert_eq!(
            watch::get(&db, &watch.id).await.unwrap().unwrap().revision,
            1
        );
    }
}
