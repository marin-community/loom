use std::collections::HashSet;

use weaver_api::SessionCatchupView;
use weaver_core::branch::Branch;
use weaver_core::{artifact, issue, tags};

use crate::auth::Principal;
use crate::session::Session;

use super::operations::{register, Bound, OperationContext};
use super::{artifact_meta, issue_views, principal_subject, require_session, ApiResult, AppState};

/// The bound `sessions.summary.get` operation.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<
        weaver_api::operations::sessions::summary::get::Get,
        _,
        _,
    >(summary_get)]
}

async fn summary_get(
    context: OperationContext,
    input: weaver_api::operations::sessions::summary::get::Input,
) -> ApiResult<SessionCatchupView> {
    let st = &context.state;
    let (session, branch) = require_session(&st.db, &input.session).await?;
    build_session_catchup(st, &context.principal, session, branch).await
}

/// Build the goal, status, inbox, artifacts, issues, and next actions for one
/// resolved session/branch.
async fn build_session_catchup(
    st: &AppState,
    principal: &Principal,
    session: Session,
    branch: Branch,
) -> ApiResult<SessionCatchupView> {
    let attention = tags::get(&st.db, &branch.id, tags::ATTENTION_KEY)
        .await?
        .map(|tag| tag.value)
        .unwrap_or_else(|| "ok".to_string());
    let channel = crate::channels::get(&st.db, &session.id, &principal_subject(principal)).await?;
    let artifacts = artifact::list_for_session(&st.db, &branch.repo_root, &branch.id)
        .await?
        .iter()
        .map(artifact_meta)
        .collect();

    let mut issues =
        issue::list_for_branch(&st.db, &branch.repo_root, &branch.branch, false).await?;
    let mut seen: HashSet<i64> = issues.iter().map(|issue| issue.id).collect();
    for backlog in issue::list_backlog(&st.db, &branch.repo_root, false).await? {
        if seen.insert(backlog.id) {
            issues.push(backlog);
        }
    }
    let issues = issue_views(&st.db, issues).await?;
    let recent_events = crate::events::history(&st.db, &branch.id, 20).await?;

    let mut next_actions = Vec::new();
    if channel
        .as_ref()
        .is_some_and(|channel| channel.unread_count > 0)
    {
        next_actions.push("loom channels read".to_string());
    }
    if !issues.is_empty() {
        next_actions.push("loom issues list".to_string());
    }
    if crate::permission_requests::list(
        &st.db,
        &session.id,
        Some(crate::permission_requests::PENDING),
    )
    .await?
    .is_empty()
    {
        next_actions.push("continue the session goal".to_string());
    } else {
        next_actions.push("loom permissions requests --state pending".to_string());
    }

    Ok(SessionCatchupView {
        session_id: session.id,
        branch_id: branch.id,
        goal: if branch.goal.is_empty() {
            branch.title
        } else {
            branch.goal
        },
        attention,
        status_message: branch.description,
        channel,
        artifacts,
        issues,
        recent_events,
        next_actions,
    })
}
