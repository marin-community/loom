use axum::{extract::State, http::HeaderMap, Extension, Json};
use weaver_api::{SelfContextLinks, SelfContextView};
use weaver_core::branch::{self, Branch};

use crate::{
    auth::{Grant, Principal},
    session::{self, Session},
};

use super::operations::{register, Bound, OperationContext};
use super::{require_session, ApiResult, AppError, AppState};

/// The bound `sessions.context` operation.
///
/// `sessions.context` was `self.get` until recently; its handler keeps living
/// here (renamed `context_get`) because `self` cannot name a Rust module, but
/// the id, route, CLI (`loom context`), and MCP (`loom_context::get`)
/// projections are all independent of that.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<
        weaver_api::operations::sessions::context::Get,
        _,
        _,
    >(context_get)]
}

async fn context_get(
    context: OperationContext,
    input: weaver_api::operations::sessions::context::Input,
) -> ApiResult<SelfContextView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    let base = super::auth::public_base(st, &HeaderMap::new()).await;
    Ok(self_context_view(&base, &session, &branch))
}

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
    Ok(Json(self_context_view(&base, &session, &branch)))
}

/// Build the caller-facing context view for one resolved session/branch pair.
/// `branch_name` carries the human name (`weaver/loom-fix-thing`) alongside
/// the id: `#[operand(context = "branch")]` fields fill from the id, but
/// `issues.backlog.create`'s `source_branch` needs the name for provenance —
/// see `ContextSource::BranchName`.
fn self_context_view(base: &str, session: &Session, branch: &Branch) -> SelfContextView {
    SelfContextView {
        session_id: session.id.clone(),
        branch_id: branch.id.clone(),
        branch_name: branch.branch.clone(),
        repo_root: branch.repo_root.clone(),
        channel_id: session.id.clone(),
        session_url: crate::links::session_url(base, &session.id),
        links: SelfContextLinks {
            channel: format!("/api/channels/{}", session.id),
            artifacts: format!("/api/branches/{}/artifacts", branch.id),
            session: format!("/api/sessions/{}", session.id),
        },
    }
}
