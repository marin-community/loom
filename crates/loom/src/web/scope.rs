//! Resource-scope checks for registered operations.
//!
//! An operation declares which resource it acts on via
//! `weaver_api::operations::Scoped`. Actor policy checks whether the caller
//! may call the operation at all; this module checks whether the caller may
//! reach this specific resource. Both run off typed input, not a URL.

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

/// A session credential may reach the branches of its own session tree.
///
/// The tree, not just the session's own branch: a session that launches a child
/// must still be able to act on what it launched. The authorization is enforced
/// against the branch operand in the operation's typed input.
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
    // Branches are addressable several ways — id, name, `repo_root:name`, an id
    // prefix — so resolve with the same function the handler will, and compare
    // rows. Matching on `id` or `branch` alone rejected `repo_root:name`, which
    // is the form the CLI builds when it polls the branch working an issue.
    let Some(resolved) = weaver_core::branch::resolve_key(&st.db, branch).await? else {
        return Err(denied("session credentials are limited to their branch"));
    };
    let resolved = resolved.id;
    if resolved == branch_id {
        return Ok(());
    }
    match session_id(principal) {
        Some(own) if super::auth::branch_belongs_to_session_tree(st, own, &resolved).await => {
            Ok(())
        }
        _ => Err(denied("session credentials are limited to their branch")),
    }
}

/// A session credential may reach its own session and its descendants,
/// so that a parent can drive the children it launched.
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
    if super::auth::is_session_descendant(st, own, session).await {
        return Ok(());
    }
    Err(denied(
        "session credentials are limited to their own session",
    ))
}
