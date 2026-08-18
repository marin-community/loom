//! Durable requests for a human to expand one live session's external access.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::Db;

pub const GITHUB_REPOSITORY_KIND: &str = "github_repository";
pub const PENDING: &str = "pending";
pub const APPROVED: &str = "approved";
pub const DENIED: &str = "denied";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum PermissionRequestState {
    Pending,
    Approved,
    Denied,
}

impl PermissionRequestState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => PENDING,
            Self::Approved => APPROVED,
            Self::Denied => DENIED,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub repository: String,
    pub mode: String,
    pub reason: String,
    pub state: PermissionRequestState,
    pub requested_by: String,
    pub requested_at: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    pub decision_reason: Option<String>,
}

const SELECT: &str = "SELECT id, session_id, kind, repository, mode, reason, state,
            requested_by, requested_at, decided_by, decided_at, decision_reason
     FROM session_permission_requests";

pub async fn create_github_repository(
    db: &Db,
    session_id: &str,
    repository: &str,
    mode: &str,
    reason: &str,
    requested_by: &str,
) -> Result<PermissionRequest> {
    let id = weaver_core::branch::new_id();
    let now = crate::db::now_iso();
    sqlx::query(
        "INSERT OR IGNORE INTO session_permission_requests
         (id, session_id, kind, repository, mode, reason, state,
          requested_by, requested_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
    )
    .bind(&id)
    .bind(session_id)
    .bind(GITHUB_REPOSITORY_KIND)
    .bind(repository)
    .bind(mode)
    .bind(reason)
    .bind(requested_by)
    .bind(&now)
    .execute(db)
    .await?;

    sqlx::query_as::<_, PermissionRequest>(&format!(
        "{SELECT} WHERE session_id = ? AND kind = ? AND repository = ?
         AND mode = ? AND state = 'pending'"
    ))
    .bind(session_id)
    .bind(GITHUB_REPOSITORY_KIND)
    .bind(repository)
    .bind(mode)
    .fetch_one(db)
    .await
    .map_err(Into::into)
}

pub async fn list(
    db: &Db,
    session_id: &str,
    state: Option<&str>,
) -> Result<Vec<PermissionRequest>> {
    let mut sql = format!("{SELECT} WHERE session_id = ?");
    if state.is_some() {
        sql.push_str(" AND state = ?");
    }
    sql.push_str(" ORDER BY requested_at DESC, id DESC");
    let query = sqlx::query_as::<_, PermissionRequest>(&sql).bind(session_id);
    match state {
        Some(state) => query.bind(state).fetch_all(db).await.map_err(Into::into),
        None => query.fetch_all(db).await.map_err(Into::into),
    }
}

pub async fn get(db: &Db, id: &str) -> Result<Option<PermissionRequest>> {
    sqlx::query_as::<_, PermissionRequest>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(Into::into)
}

/// Approve a pending GitHub request and apply the reviewed repository grant in
/// the same database transaction. External installation validation happens
/// before this function is called.
pub async fn approve_github(
    db: &Db,
    id: &str,
    decided_by: &str,
    decision_reason: &str,
) -> Result<bool> {
    let Some(request) = get(db, id).await? else {
        return Ok(false);
    };
    if request.state != PermissionRequestState::Pending {
        return Ok(false);
    }
    let now = crate::db::now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let changed = sqlx::query(
        "UPDATE session_permission_requests SET state = 'approved',
             decided_by = ?, decided_at = ?, decision_reason = ?
         WHERE id = ? AND state = 'pending'",
    )
    .bind(decided_by)
    .bind(&now)
    .bind(decision_reason)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 0 {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO session_github_access
             (session_id, repository, mode, granted_by, granted_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(session_id, repository) DO UPDATE SET
             mode = excluded.mode,
             granted_by = excluded.granted_by,
             granted_at = excluded.granted_at",
    )
    .bind(&request.session_id)
    .bind(&request.repository)
    .bind(&request.mode)
    .bind(decided_by)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn deny(db: &Db, id: &str, decided_by: &str, decision_reason: &str) -> Result<bool> {
    let changed = sqlx::query(
        "UPDATE session_permission_requests SET state = 'denied',
             decided_by = ?, decided_at = ?, decision_reason = ?
         WHERE id = ? AND state = 'pending'",
    )
    .bind(decided_by)
    .bind(crate::db::now_iso())
    .bind(decision_reason)
    .bind(id)
    .execute(db)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seeded_db() -> Db {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/access", "main")
            .await
            .unwrap();
        crate::session::insert(
            &db,
            &crate::session::NewSession {
                id: "session".to_string(),
                branch_id: branch.id,
                work_dir: "/work".to_string(),
                term_session: "term".to_string(),
                agent_kind: "codex".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: Some("alice".to_string()),
                protocol: "acp".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn duplicate_pending_scope_returns_the_existing_request() {
        let db = seeded_db().await;
        let first = create_github_repository(
            &db,
            "session",
            "acme/widgets",
            "write",
            "open a pull request",
            "session",
        )
        .await
        .unwrap();
        let second =
            create_github_repository(&db, "session", "acme/widgets", "write", "retry", "session")
                .await
                .unwrap();
        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn approval_applies_grant_atomically() {
        let db = seeded_db().await;
        let request = create_github_repository(
            &db,
            "session",
            "acme/widgets",
            "write",
            "open a pull request",
            "session",
        )
        .await
        .unwrap();
        assert!(approve_github(&db, &request.id, "alice", "approved")
            .await
            .unwrap());
        assert_eq!(
            get(&db, &request.id).await.unwrap().unwrap().state,
            PermissionRequestState::Approved
        );
        assert_eq!(
            crate::github_access::list(&db, "session")
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
