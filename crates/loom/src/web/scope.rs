//! Resource-scope checks for registered operations.
//!
//! An operation declares *which* resource it acts on through [`Scoped`], and
//! this module answers whether the caller may reach that instance. Splitting the
//! question in two — actor answers "may you call this at all", scope answers
//! "may you reach this one" — is what lets authorization be decided from typed
//! input instead of by matching a URL.

use axum::http::StatusCode;

use crate::auth::{Grant, Principal};
use crate::AppState;

use super::{ApiResult, AppError};

fn denied(detail: &'static str) -> AppError {
    AppError::new(StatusCode::FORBIDDEN, detail)
}

/// The branch a session credential is bound to, if it is one.
fn session_branch(principal: &Principal) -> Option<&str> {
    match &principal.grant {
        Grant::Session { branch_id, .. } => Some(branch_id.as_str()),
        _ => None,
    }
}

fn session_id(principal: &Principal) -> Option<&str> {
    match &principal.grant {
        Grant::Session { session_id, .. } => Some(session_id.as_str()),
        _ => None,
    }
}

/// A session credential may only reach its own repository.
pub(crate) async fn require_repo_access(
    st: &AppState,
    principal: &Principal,
    repo_root: &str,
) -> ApiResult<()> {
    let Some(branch_id) = session_branch(principal) else {
        return Ok(());
    };
    let own: Option<String> = sqlx::query_scalar("SELECT repo_root FROM branches WHERE id = ?")
        .bind(branch_id)
        .fetch_optional(&st.db)
        .await?;
    if own.as_deref() == Some(repo_root) {
        Ok(())
    } else {
        Err(denied(
            "session credentials are limited to their repository",
        ))
    }
}

/// A session credential may only reach its own branch.
pub(crate) async fn require_branch_access(
    st: &AppState,
    principal: &Principal,
    branch: &str,
) -> ApiResult<()> {
    let Some(branch_id) = session_branch(principal) else {
        return Ok(());
    };
    if branch_id == branch {
        return Ok(());
    }
    // Branches are addressable by id or by name within the session's repo; both
    // resolve to the same row, so compare after resolution rather than rejecting
    // a legitimate alias.
    let resolved: Option<String> =
        sqlx::query_scalar("SELECT id FROM branches WHERE id = ? OR name = ?")
            .bind(branch)
            .bind(branch)
            .fetch_optional(&st.db)
            .await?;
    if resolved.as_deref() == Some(branch_id) {
        Ok(())
    } else {
        Err(denied("session credentials are limited to their branch"))
    }
}

/// A session credential may only reach its own session.
pub(crate) async fn require_session_access(
    st: &AppState,
    principal: &Principal,
    session: &str,
) -> ApiResult<()> {
    let Some(own) = session_id(principal) else {
        return Ok(());
    };
    // `self` is the ordinary way an agent names its own session.
    if session.is_empty() || session == "self" || session == own {
        return Ok(());
    }
    let resolved: Option<String> = sqlx::query_scalar("SELECT id FROM sessions WHERE id = ?")
        .bind(session)
        .fetch_optional(&st.db)
        .await?;
    if resolved.as_deref() == Some(own) {
        Ok(())
    } else {
        Err(denied(
            "session credentials are limited to their own session",
        ))
    }
}
