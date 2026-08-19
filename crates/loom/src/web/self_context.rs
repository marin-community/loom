use axum::http::HeaderMap;
use weaver_api::operations::{artifacts, channels, sessions, Operation};
use weaver_api::{SelfContextLinks, SelfContextView};
use weaver_core::branch::Branch;

use crate::session::Session;

use super::operations::{register, Bound, OperationContext};
use super::{require_session, ApiResult};

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
        // The links carry no ids any more, and that is the point: each names an
        // operation whose one operand is this caller's own context, so a session
        // credential POSTing `{}` to it gets its own channel, its own artifacts,
        // its own session. The old per-id URLs were the same three reads spelled
        // as routes that no longer exist.
        links: SelfContextLinks {
            channel: channels::get::Get::SPEC.path().to_string(),
            artifacts: artifacts::list::List::SPEC.path().to_string(),
            session: sessions::get::Get::SPEC.path().to_string(),
        },
    }
}
