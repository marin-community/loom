use std::collections::HashSet;

use axum::{
    extract::{Path, State},
    Extension, Json,
};
use weaver_api::SessionCatchupView;
use weaver_core::{artifact, issue, tags};

use crate::auth::Principal;

use super::{artifact_meta, issue_views, principal_subject, require_session, ApiResult, AppState};

/// Structured, server-authoritative catch-up for one session. This replaces
/// client-side orchestration in the old `weaver summary` command.
pub(super) async fn get_session_summary(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<SessionCatchupView>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    let attention = tags::get(&st.db, &branch.id, tags::ATTENTION_KEY)
        .await?
        .map(|tag| tag.value)
        .unwrap_or_else(|| "ok".to_string());
    let channel = crate::channels::get(&st.db, &session.id, &principal_subject(&principal)).await?;
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

    Ok(Json(SessionCatchupView {
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
    }))
}
