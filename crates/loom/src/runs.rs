//! Durable, idempotent automation-run reservations.

use anyhow::Result;
use sqlx::FromRow;
use weaver_api::RunView;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, FromRow)]
pub struct Run {
    pub id: String,
    pub actor_subject: String,
    pub source: String,
    pub service_tag: String,
    pub profile: String,
    pub idempotency_key: String,
    pub channel: Option<String>,
    pub request_json: String,
    pub session_id: String,
    pub status: String,
    pub outcome: Option<String>,
    pub summary: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<Run> for RunView {
    fn from(run: Run) -> Self {
        Self {
            id: run.id,
            actor_subject: run.actor_subject,
            source: run.source,
            service_tag: run.service_tag,
            profile: run.profile,
            idempotency_key: run.idempotency_key,
            channel: run.channel,
            session_id: run.session_id,
            status: run.status,
            outcome: run.outcome,
            summary: run.summary,
            created_at: run.created_at,
            updated_at: run.updated_at,
        }
    }
}

pub enum Reservation {
    Created(Run),
    Existing(Run),
}

pub struct NewRun<'a> {
    pub subject: &'a str,
    pub source: &'a str,
    pub service_tag: &'a str,
    pub profile: &'a str,
    pub idempotency_key: &'a str,
    pub channel: Option<&'a str>,
    pub request_json: &'a str,
}

pub enum ChannelAction {
    Launch(Run),
    Prompt(Run),
    Ready(Run),
    Busy(Run),
}

#[derive(FromRow)]
struct ChannelOwner {
    owner_run_id: String,
    session_id: String,
    run_status: String,
    run_updated_at: String,
    session_status: Option<String>,
    session_protocol: Option<String>,
}

pub fn validate_channel(channel: &str) -> Result<()> {
    if channel.is_empty()
        || channel.len() > 64
        || !channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        anyhow::bail!("channel must be 1-64 ASCII letters, digits, '.', '_', ':', or '-'");
    }
    Ok(())
}

pub async fn reserve(db: &Db, request: NewRun<'_>) -> Result<Reservation> {
    let id = weaver_core::branch::new_id();
    let session_id = weaver_core::branch::new_id();
    let now = now_iso();
    let result = sqlx::query(
        "INSERT INTO automation_runs
         (id, actor_subject, source, service_tag, profile, idempotency_key, channel,
          request_json, session_id, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'creating', ?, ?)
         ON CONFLICT(actor_subject, idempotency_key) DO NOTHING",
    )
    .bind(&id)
    .bind(request.subject)
    .bind(request.source)
    .bind(request.service_tag)
    .bind(request.profile)
    .bind(request.idempotency_key)
    .bind(request.channel)
    .bind(request.request_json)
    .bind(&session_id)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    let run = get_by_key(db, request.subject, request.idempotency_key)
        .await?
        .expect("inserted or conflicting automation run exists");
    Ok(if result.rows_affected() == 1 {
        Reservation::Created(run)
    } else {
        Reservation::Existing(run)
    })
}

pub async fn route_channel(db: &Db, run_id: &str) -> Result<ChannelAction> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let mut run = sqlx::query_as::<_, Run>("SELECT * FROM automation_runs WHERE id = ?")
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?;
    let channel = run
        .channel
        .clone()
        .ok_or_else(|| anyhow::anyhow!("automation run has no channel"))?;
    validate_channel(&channel)?;

    let owner = sqlx::query_as::<_, ChannelOwner>(
        "SELECT c.owner_run_id, c.session_id, r.status AS run_status,
                r.updated_at AS run_updated_at, s.status AS session_status,
                s.protocol AS session_protocol
         FROM automation_channels c
         JOIN automation_runs r ON r.id = c.owner_run_id
         LEFT JOIN sessions s ON s.id = c.session_id
         WHERE c.actor_subject = ? AND c.source = ? AND c.service_tag = ?
           AND c.profile = ? AND c.channel = ?",
    )
    .bind(&run.actor_subject)
    .bind(&run.source)
    .bind(&run.service_tag)
    .bind(&run.profile)
    .bind(&channel)
    .fetch_optional(&mut *tx)
    .await?;

    let action = match owner {
        None => {
            sqlx::query(
                "INSERT INTO automation_channels
                 (actor_subject, source, service_tag, profile, channel, owner_run_id,
                  session_id, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&run.actor_subject)
            .bind(&run.source)
            .bind(&run.service_tag)
            .bind(&run.profile)
            .bind(&channel)
            .bind(&run.id)
            .bind(&run.session_id)
            .bind(now_iso())
            .execute(&mut *tx)
            .await?;
            ChannelAction::Launch(run)
        }
        Some(owner)
            if owner.session_status.as_deref() == Some("running")
                && owner.session_protocol.as_deref() == Some("acp") =>
        {
            if owner.owner_run_id == run.id {
                sqlx::query(
                    "UPDATE automation_runs SET status = 'running', session_id = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&owner.session_id)
                .bind(now_iso())
                .bind(&run.id)
                .execute(&mut *tx)
                .await?;
                run.session_id = owner.session_id;
                run.status = "running".to_string();
                ChannelAction::Ready(run)
            } else {
                sqlx::query(
                    "UPDATE automation_runs
                     SET status = 'delivering', session_id = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&owner.session_id)
                .bind(now_iso())
                .bind(&run.id)
                .execute(&mut *tx)
                .await?;
                run.session_id = owner.session_id;
                run.status = "delivering".to_string();
                ChannelAction::Prompt(run)
            }
        }
        Some(owner)
            if owner.owner_run_id == run.id
                && owner.session_status.is_none()
                && owner.run_status == "creating"
                && owner.run_updated_at > stale_before() =>
        {
            ChannelAction::Busy(run)
        }
        Some(owner)
            if matches!(
                owner.session_status.as_deref(),
                Some("created" | "orphaned")
            ) || (owner.session_status.is_none()
                && owner.run_status == "creating"
                && owner.run_updated_at > stale_before()) =>
        {
            sqlx::query(
                "UPDATE automation_runs SET status = 'waiting', updated_at = ? WHERE id = ?",
            )
            .bind(now_iso())
            .bind(&run.id)
            .execute(&mut *tx)
            .await?;
            run.status = "waiting".to_string();
            ChannelAction::Busy(run)
        }
        Some(_) => {
            let session_id = weaver_core::branch::new_id();
            sqlx::query(
                "UPDATE automation_channels
                 SET owner_run_id = ?, session_id = ?, updated_at = ?
                 WHERE actor_subject = ? AND source = ? AND service_tag = ?
                   AND profile = ? AND channel = ?",
            )
            .bind(&run.id)
            .bind(&session_id)
            .bind(now_iso())
            .bind(&run.actor_subject)
            .bind(&run.source)
            .bind(&run.service_tag)
            .bind(&run.profile)
            .bind(&channel)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE automation_runs
                 SET status = 'creating', session_id = ?, updated_at = ?
                 WHERE id = ?",
            )
            .bind(&session_id)
            .bind(now_iso())
            .bind(&run.id)
            .execute(&mut *tx)
            .await?;
            run.session_id = session_id;
            run.status = "creating".to_string();
            ChannelAction::Launch(run)
        }
    };
    tx.commit().await?;
    Ok(action)
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Run>> {
    Ok(
        sqlx::query_as::<_, Run>("SELECT * FROM automation_runs WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

async fn get_by_key(db: &Db, subject: &str, key: &str) -> Result<Option<Run>> {
    Ok(sqlx::query_as::<_, Run>(
        "SELECT * FROM automation_runs WHERE actor_subject = ? AND idempotency_key = ?",
    )
    .bind(subject)
    .bind(key)
    .fetch_optional(db)
    .await?)
}

pub async fn list_for(db: &Db, subject: Option<&str>) -> Result<Vec<Run>> {
    match subject {
        Some(subject) => Ok(sqlx::query_as::<_, Run>(
            "SELECT * FROM automation_runs WHERE actor_subject = ? ORDER BY created_at DESC",
        )
        .bind(subject)
        .fetch_all(db)
        .await?),
        None => Ok(sqlx::query_as::<_, Run>(
            "SELECT * FROM automation_runs ORDER BY created_at DESC",
        )
        .fetch_all(db)
        .await?),
    }
}

pub async fn launched(db: &Db, id: &str, session_id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE automation_runs SET status = 'running', updated_at = ?
         WHERE id = ? AND session_id = ?",
    )
    .bind(now_iso())
    .bind(id)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Claim a reservation abandoned while provisioning. A live request keeps its
/// five-minute lease; after that, exactly one retry may resume with the same
/// preallocated session id.
pub async fn claim_stale(db: &Db, id: &str) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE automation_runs SET updated_at = ?
         WHERE id = ? AND status = 'creating' AND updated_at <= ?",
    )
    .bind(now_iso())
    .bind(id)
    .bind(stale_before())
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

pub async fn claim_stale_delivery(db: &Db, id: &str) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE automation_runs SET status = 'waiting', updated_at = ?
         WHERE id = ? AND status = 'delivering' AND updated_at <= ?",
    )
    .bind(now_iso())
    .bind(id)
    .bind(stale_before())
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

pub async fn waiting(db: &Db, id: &str) -> Result<()> {
    sqlx::query("UPDATE automation_runs SET status = 'waiting', updated_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn failed(db: &Db, id: &str, summary: &str) -> Result<()> {
    sqlx::query(
        "UPDATE automation_runs SET status = 'failed', outcome = 'failed', summary = ?, updated_at = ? WHERE id = ?",
    )
    .bind(summary)
    .bind(now_iso())
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

fn stale_before() -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Close the durable run when its session is deliberately removed. Keeping the
/// reservation preserves idempotency/audit history; changing its status keeps
/// it out of the active provisioning queue.
pub async fn cancel_for_session(db: &Db, session_id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE automation_runs
         SET status = 'cancelled', outcome = 'cancelled',
             summary = 'session removed by user', updated_at = ?
         WHERE session_id = ?
           AND status IN ('creating', 'waiting', 'delivering', 'running', 'failed')",
    )
    .bind(now_iso())
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Repair reservations whose provisioning request or session disappeared.
/// A fresh `creating` lease may still belong to an older server draining during
/// a rolling restart, so only the same five-minute stale lease accepted by
/// [`claim_stale`] is abandoned. A `running` run must already have inserted its
/// session and is inconsistent immediately when that row is absent.
pub async fn reconcile_missing_sessions(db: &Db) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE automation_runs
         SET status = 'cancelled', outcome = 'cancelled',
             summary = 'session provisioning was interrupted', updated_at = ?
         WHERE (status = 'running' OR (status = 'creating' AND updated_at <= ?))
           AND NOT EXISTS (
               SELECT 1 FROM sessions WHERE sessions.id = automation_runs.session_id
           )",
    )
    .bind(now_iso())
    .bind(stale_before())
    .execute(db)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reservation_is_idempotent_for_subject_and_key() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let first = reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "actions",
                service_tag: "weaver-actions",
                profile: "default",
                idempotency_key: "delivery",
                channel: None,
                request_json: "{}",
            },
        )
        .await
        .unwrap();
        let (first_id, first_session_id) = match first {
            Reservation::Created(run) => (run.id, run.session_id),
            Reservation::Existing(_) => panic!("first reservation must be new"),
        };
        let second = reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "actions",
                service_tag: "weaver-actions",
                profile: "default",
                idempotency_key: "delivery",
                channel: None,
                request_json: "different",
            },
        )
        .await
        .unwrap();
        match second {
            Reservation::Existing(run) => {
                assert_eq!(run.id, first_id);
                assert_eq!(run.session_id, first_session_id);
            }
            Reservation::Created(_) => panic!("retry created a duplicate"),
        }
        assert_eq!(list_for(&db, None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn only_a_stale_creating_run_can_be_reclaimed() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let run = match reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "actions",
                service_tag: "weaver-actions",
                profile: "default",
                idempotency_key: "key",
                channel: None,
                request_json: "{}",
            },
        )
        .await
        .unwrap()
        {
            Reservation::Created(run) => run,
            Reservation::Existing(_) => unreachable!(),
        };
        assert!(!claim_stale(&db, &run.id).await.unwrap());
        sqlx::query("UPDATE automation_runs SET updated_at = '2000-01-01T00:00:00.000Z'")
            .execute(&db)
            .await
            .unwrap();
        assert!(claim_stale(&db, &run.id).await.unwrap());
        assert!(!claim_stale(&db, &run.id).await.unwrap());
    }
    #[tokio::test]
    async fn channel_waits_for_its_owner_and_replaces_a_failed_launch() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let first = match reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "grafana",
                service_tag: "grafana",
                profile: "default",
                idempotency_key: "first",
                channel: Some("operator"),
                request_json: "{}",
            },
        )
        .await
        .unwrap()
        {
            Reservation::Created(run) => run,
            Reservation::Existing(_) => unreachable!(),
        };
        assert!(matches!(
            route_channel(&db, &first.id).await.unwrap(),
            ChannelAction::Launch(_)
        ));

        let second = match reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "grafana",
                service_tag: "grafana",
                profile: "default",
                idempotency_key: "second",
                channel: Some("operator"),
                request_json: "{}",
            },
        )
        .await
        .unwrap()
        {
            Reservation::Created(run) => run,
            Reservation::Existing(_) => unreachable!(),
        };
        assert!(matches!(
            route_channel(&db, &second.id).await.unwrap(),
            ChannelAction::Busy(_)
        ));

        failed(&db, &first.id, "launch failed").await.unwrap();
        assert!(matches!(
            route_channel(&db, &second.id).await.unwrap(),
            ChannelAction::Launch(_)
        ));
    }

    #[tokio::test]
    async fn startup_reconciles_reservations_without_sessions() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let run = match reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "ops",
                service_tag: "grafana",
                profile: "default",
                idempotency_key: "missing-session",
                channel: None,
                request_json: "{}",
            },
        )
        .await
        .unwrap()
        {
            Reservation::Created(run) => run,
            Reservation::Existing(_) => unreachable!(),
        };
        launched(&db, &run.id, &run.session_id).await.unwrap();

        assert_eq!(reconcile_missing_sessions(&db).await.unwrap(), 1);
        let repaired = get(&db, &run.id).await.unwrap().unwrap();
        assert_eq!(repaired.status, "cancelled");
        assert_eq!(repaired.outcome.as_deref(), Some("cancelled"));
        assert_eq!(repaired.summary, "session provisioning was interrupted");
    }

    #[tokio::test]
    async fn removing_a_session_cancels_its_durable_run() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let run = match reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "ops",
                service_tag: "grafana",
                profile: "default",
                idempotency_key: "removed-session",
                channel: None,
                request_json: "{}",
            },
        )
        .await
        .unwrap()
        {
            Reservation::Created(run) => run,
            Reservation::Existing(_) => unreachable!(),
        };
        launched(&db, &run.id, &run.session_id).await.unwrap();

        cancel_for_session(&db, &run.session_id).await.unwrap();
        let cancelled = get(&db, &run.id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.outcome.as_deref(), Some("cancelled"));
        assert_eq!(cancelled.summary, "session removed by user");
    }

    #[tokio::test]
    async fn startup_preserves_fresh_creating_leases_but_cancels_stale_ones() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let run = match reserve(
            &db,
            NewRun {
                subject: "subject",
                source: "ops",
                service_tag: "grafana",
                profile: "default",
                idempotency_key: "creating-session",
                channel: None,
                request_json: "{}",
            },
        )
        .await
        .unwrap()
        {
            Reservation::Created(run) => run,
            Reservation::Existing(_) => unreachable!(),
        };

        assert_eq!(reconcile_missing_sessions(&db).await.unwrap(), 0);
        sqlx::query("UPDATE automation_runs SET updated_at = '2000-01-01T00:00:00.000Z'")
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(reconcile_missing_sessions(&db).await.unwrap(), 1);
        assert_eq!(
            get(&db, &run.id).await.unwrap().unwrap().status,
            "cancelled"
        );
    }
}
