use axum::{extract::State, http::HeaderMap, Extension, Json};
use weaver_api::{SelfContextLinks, SelfContextView};
use weaver_core::branch;

use crate::{
    auth::{Grant, Principal},
    session,
};

use super::{ApiResult, AppError, AppState};

/// `GET /api/self` — resolve the caller's canonical session, branch, channel,
/// and links once. Agent-facing tools accept `self`; REST resources remain
/// id-addressed through the returned links.
pub(super) async fn get_self_context(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
) -> ApiResult<Json<SelfContextView>> {
    let Grant::Session {
        session_id,
        branch_id,
        ..
    } = &principal.grant
    else {
        return Err(AppError::bad_request(
            "the current credential is not bound to a session",
        ));
    };
    let session = session::get(&st.db, session_id)
        .await?
        .ok_or_else(|| AppError::not_found("session"))?;
    let branch = branch::get(&st.db, branch_id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    let base = super::auth::public_base(&st, &headers).await;
    Ok(Json(SelfContextView {
        session_id: session.id.clone(),
        branch_id: branch.id.clone(),
        branch_name: branch.branch.clone(),
        repo_root: branch.repo_root,
        channel_id: session.id.clone(),
        session_url: crate::links::session_url(&base, &session.id),
        links: SelfContextLinks {
            channel: format!("/api/channels/{}", session.id),
            artifacts: format!("/api/branches/{}/artifacts", branch.id),
            session: format!("/api/sessions/{}", session.id),
        },
    }))
}
