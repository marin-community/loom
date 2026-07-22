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
    pub profile: String,
    pub idempotency_key: String,
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
            profile: run.profile,
            idempotency_key: run.idempotency_key,
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

pub async fn reserve(
    db: &Db,
    subject: &str,
    source: &str,
    profile: &str,
    idempotency_key: &str,
    request_json: &str,
) -> Result<Reservation> {
    let id = weaver_core::branch::new_id();
    let session_id = weaver_core::branch::new_id();
    let now = now_iso();
    let result = sqlx::query(
        "INSERT INTO automation_runs
         (id, actor_subject, source, profile, idempotency_key, request_json,
          session_id, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'creating', ?, ?)
         ON CONFLICT(actor_subject, idempotency_key) DO NOTHING",
    )
    .bind(&id)
    .bind(subject)
    .bind(source)
    .bind(profile)
    .bind(idempotency_key)
    .bind(request_json)
    .bind(&session_id)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    let run = get_by_key(db, subject, idempotency_key)
        .await?
        .expect("inserted or conflicting automation run exists");
    Ok(if result.rows_affected() == 1 {
        Reservation::Created(run)
    } else {
        Reservation::Existing(run)
    })
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
    let stale_before = (chrono::Utc::now() - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Ok(sqlx::query(
        "UPDATE automation_runs SET updated_at = ?
         WHERE id = ? AND status = 'creating' AND updated_at <= ?",
    )
    .bind(now_iso())
    .bind(id)
    .bind(stale_before)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reservation_is_idempotent_for_subject_and_key() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let first = reserve(&db, "subject", "actions", "default", "delivery", "{}")
            .await
            .unwrap();
        let (first_id, first_session_id) = match first {
            Reservation::Created(run) => (run.id, run.session_id),
            Reservation::Existing(_) => panic!("first reservation must be new"),
        };
        let second = reserve(
            &db,
            "subject",
            "actions",
            "default",
            "delivery",
            "different",
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
        let run = match reserve(&db, "subject", "actions", "default", "key", "{}")
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
}
