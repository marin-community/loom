//! Human approval workflow for session-scoped external access expansions.

use serde_json::json;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::{EffectivePermissionsView, PermissionRequestView};
use weaver_core::{branch as branch_mod, tags};

use crate::{
    auth::Principal,
    channels::{MessageKind, NewMessage, Subject, SubjectKind, Urgency},
    permission_requests,
};

use super::operations::{register, Bound, OperationContext};
use super::{
    channels::{append_and_deliver, record_channel_message_event},
    effective_repositories, github_token_operation, grant_github_access_operation,
    policy_repository_patterns, require_session, restricted_github_invoke_operation,
    revoke_github_access_operation, validate_github_write, ApiResult, AppError, AppState,
};
use weaver_api::operations::channels;

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<permission_operations::explain::Op, _, _>(explain_operation),
        register::<permission_operations::effective::get::Op, _, _>(
            effective_permissions_operation,
        ),
        register::<permission_operations::requests::list::Op, _, _>(
            list_permission_requests_operation,
        ),
        register::<permission_operations::requests::create::Op, _, _>(
            create_permission_request_operation,
        ),
        register::<permission_operations::requests::approve::Op, _, _>(
            approve_permission_request_operation,
        ),
        register::<permission_operations::requests::deny::Op, _, _>(
            deny_permission_request_operation,
        ),
        register::<permission_operations::github::token::Op, _, _>(github_token_operation),
        register::<permission_operations::github::grant::Op, _, _>(grant_github_access_operation),
        register::<permission_operations::github::revoke::Op, _, _>(revoke_github_access_operation),
        register::<permission_operations::github::restricted::invoke::Op, _, _>(
            restricted_github_invoke_operation,
        ),
    ]
}

fn view(request: permission_requests::PermissionRequest) -> PermissionRequestView {
    PermissionRequestView {
        id: request.id,
        session_id: request.session_id,
        kind: request.kind,
        repository: request.repository,
        mode: request.mode,
        reason: request.reason,
        state: request.state.as_str().to_string(),
        requested_by: request.requested_by,
        requested_at: request.requested_at,
        decided_by: request.decided_by,
        decided_at: request.decided_at,
        decision_reason: request.decision_reason,
    }
}

fn validate_state(state: Option<&str>) -> ApiResult<Option<&str>> {
    match state.map(str::trim).filter(|state| !state.is_empty()) {
        None => Ok(None),
        Some(state)
            if matches!(
                state,
                permission_requests::PENDING
                    | permission_requests::APPROVED
                    | permission_requests::DENIED
            ) =>
        {
            Ok(Some(state))
        }
        Some(_) => Err(AppError::bad_request(
            "state must be pending, approved, or denied",
        )),
    }
}

pub(super) async fn explain_operation(
    _context: OperationContext,
    input: permission_operations::explain::Input,
) -> ApiResult<permission_operations::explain::Output> {
    weaver_api::operation(&input.operation)
        .map(weaver_api::OperationView::from)
        .ok_or_else(|| AppError::not_found("operation"))
}

pub(super) async fn effective_permissions_operation(
    context: OperationContext,
    input: permission_operations::effective::get::Input,
) -> ApiResult<EffectivePermissionsView> {
    let st = context.state;
    let key = input.session;
    let (session, _) = require_session(&st.db, &key).await?;
    let github_repositories = effective_repositories(&st.db, &session)
        .await
        .map_err(|error| AppError::internal("could not resolve GitHub access", error))?;
    let github_repository_patterns = policy_repository_patterns(&session)
        .map_err(|error| AppError::internal("could not resolve GitHub access", error))?;
    let pending_requests =
        permission_requests::list(&st.db, &session.id, Some(permission_requests::PENDING))
            .await?
            .into_iter()
            .map(view)
            .collect();
    let capabilities = crate::auth::session_capabilities_for_policy(
        session.policy_restricted,
        &session.policy_mcp_access,
    )
    .map_err(|error| AppError::internal("could not resolve operation access", error))?;
    let operations = weaver_api::operations()
        .filter(|operation| {
            operation.actor == weaver_api::ActorPolicy::SessionSelf
                && operation
                    .grants
                    .iter()
                    .all(|required| capabilities.iter().any(|value| value == required))
        })
        .map(|operation| operation.id.to_string())
        .collect();
    Ok(EffectivePermissionsView {
        session_id: session.id,
        actor: "session_self".to_string(),
        operations,
        github_repositories,
        github_repository_patterns,
        pending_requests,
    })
}

pub(super) async fn list_permission_requests_operation(
    context: OperationContext,
    input: permission_operations::requests::list::Input,
) -> ApiResult<Vec<PermissionRequestView>> {
    let st = context.state;
    let key = input.session;
    let (session, _) = require_session(&st.db, &key).await?;
    let state = validate_state(input.state.as_deref())?;
    Ok(permission_requests::list(&st.db, &session.id, state)
        .await?
        .into_iter()
        .map(view)
        .collect())
}

/// Record the durable audit event for one resolved access request. Both the
/// human decision and the launch-policy grant land the same event; they differ
/// only in who decided and how the session is told.
async fn record_decision_event(
    st: &AppState,
    branch_id: &str,
    decided: &permission_requests::PermissionRequest,
    decision: &str,
    by: &str,
) -> ApiResult<()> {
    crate::events::record(
        &st.db,
        &st.bus,
        branch_id,
        "permission_decision",
        json!({
            "request_id": decided.id,
            "decision": decision,
            "repository": decided.repository,
            "by": by,
        }),
    )
    .await?;
    Ok(())
}

/// Apply an expansion the launch policy already authorizes. Same durable
/// record and audit trail as a human decision, minus the wait: the session
/// channel gets an ordinary informational message and no attention is raised.
async fn apply_policy_grant(
    st: &AppState,
    session: &crate::session::Session,
    branch_id: &str,
    request: permission_requests::PermissionRequest,
    pattern: &str,
) -> ApiResult<PermissionRequestView> {
    let decided_by = format!("policy:{pattern}");
    let decision_reason = format!("launch policy allowlists {pattern}");
    if !permission_requests::approve_github(&st.db, &request.id, &decided_by, &decision_reason)
        .await?
    {
        return Err(AppError::conflict("permission request is already resolved"));
    }
    let decided = permission_requests::get(&st.db, &request.id)
        .await?
        .ok_or_else(|| AppError::not_found("permission request"))?;
    let author = Subject::new(SubjectKind::Session, &session.id);
    crate::channels::append(
        &st.db,
        &session.id,
        NewMessage {
            kind: MessageKind::System,
            urgency: Urgency::Normal,
            author: &author,
            body: &format!(
                "GitHub write access granted for {} by launch policy ({pattern}).",
                decided.repository
            ),
            payload: &json!({
                "permission_request_id": decided.id,
                "decision": "approve",
                "repository": decided.repository,
                "pattern": pattern,
            }),
            reply_to: None,
            idempotency_key: Some(&format!("permission-decision:{}", decided.id)),
        },
    )
    .await?;
    record_decision_event(st, branch_id, &decided, "approve", &decided_by).await?;
    Ok(view(decided))
}

pub(super) async fn create_permission_request_operation(
    context: OperationContext,
    input: permission_operations::requests::create::Input,
) -> ApiResult<PermissionRequestView> {
    let st = context.state;
    let principal = context.principal;
    let key = input.session;
    let (session, branch) = require_session(&st.db, &key).await?;
    if input.mode.trim() != "write" {
        return Err(AppError::bad_request("mode must be 'write'"));
    }
    let repository = crate::repo::parse_slug(input.repository.trim())
        .map_err(AppError::bad_request)?
        .slug();
    let reason = input.reason.trim();
    if reason.is_empty() {
        return Err(AppError::bad_request("reason is required"));
    }
    if reason.len() > permission_operations::MAX_REASON_LEN {
        return Err(AppError::bad_request(format!(
            "reason exceeds {} bytes",
            permission_operations::MAX_REASON_LEN
        )));
    }
    if effective_repositories(&st.db, &session)
        .await
        .map_err(|error| AppError::internal("could not resolve GitHub access", error))?
        .contains(&repository)
    {
        return Err(AppError::conflict(format!(
            "session already has write access to {repository}"
        )));
    }
    let requested_by = match &principal.grant {
        crate::auth::Grant::Session { session_id, .. } => format!("session:{session_id}"),
        _ => principal.username.clone(),
    };
    let request = permission_requests::create_github_repository(
        &st.db,
        &session.id,
        &repository,
        "write",
        reason,
        &requested_by,
    )
    .await?;

    // An `owner/*` entry on the launch policy is a standing human decision for
    // that owner, so a matching request is granted immediately rather than
    // waiting on a human. The GitHub App installation is still validated, and
    // the grant is still recorded and revocable. If the App cannot grant it,
    // fall through to human approval — installing the App later unblocks it.
    let patterns = policy_repository_patterns(&session)
        .map_err(|error| AppError::internal("could not resolve GitHub access", error))?;
    if let Some(pattern) = patterns
        .iter()
        .find(|pattern| crate::runtime::pattern_allows(std::slice::from_ref(pattern), &repository))
    {
        match validate_github_write(&st, &repository).await {
            Ok(()) => {
                return apply_policy_grant(&st, &session, &branch.id, request, pattern).await;
            }
            Err(error) => tracing::warn!(
                session = %session.id, %repository, pattern = %pattern, detail = %error.message,
                "launch policy allows this expansion but the GitHub App cannot grant it; \
                 falling back to human approval"
            ),
        }
    }

    let message = format!("GitHub write access requested for {repository}: {reason}");
    branch_mod::set_description(&st.db, &branch.id, &message).await?;
    tags::set(
        &st.db,
        &branch.id,
        tags::ATTENTION_KEY,
        "attention",
        "",
        "agent",
    )
    .await?;
    let author = Subject::new(SubjectKind::Session, &session.id);
    crate::channels::append(
        &st.db,
        &session.id,
        NewMessage {
            kind: MessageKind::System,
            urgency: Urgency::Attention,
            author: &author,
            body: &message,
            payload: &json!({
                "permission_request_id": request.id,
                "kind": request.kind,
                "repository": request.repository,
                "mode": request.mode,
            }),
            reply_to: None,
            idempotency_key: Some(&format!("permission-request:{}", request.id)),
        },
    )
    .await?;
    crate::events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "permission_request",
        json!({
            "request_id": request.id,
            "kind": request.kind,
            "repository": request.repository,
            "mode": request.mode,
            "by": requested_by,
        }),
    )
    .await?;
    crate::slack::spawn_status_mirrors(st.clone(), branch.id);
    Ok(view(request))
}

/// Shared body for `permissions.requests.approve` and `permissions.requests.deny`.
///
/// Both operations resolve to the same durable transition — a pending request
/// moving to a terminal state — differing only in which storage call runs and
/// how the notification reads. Which humans may reach this at all is decided
/// centrally from `actor = User` on each declaration; nothing here re-checks
/// who the caller is.
async fn apply_permission_decision(
    st: &AppState,
    principal: &Principal,
    id: &str,
    decision: &'static str,
    reason: &str,
) -> ApiResult<PermissionRequestView> {
    let request = permission_requests::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("permission request"))?;
    if request.state != permission_requests::PermissionRequestState::Pending {
        return Err(AppError::conflict("permission request is already resolved"));
    }
    let (session, branch) = require_session(&st.db, &request.session_id).await?;
    let changed = match decision {
        "approve" => {
            validate_github_write(st, &request.repository).await?;
            permission_requests::approve_github(&st.db, id, &principal.username, reason).await?
        }
        "deny" => permission_requests::deny(&st.db, id, &principal.username, reason).await?,
        _ => unreachable!("decision is fixed by the calling operation"),
    };
    if !changed {
        return Err(AppError::conflict("permission request is already resolved"));
    }
    let decided = permission_requests::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("permission request"))?;
    let body = if decision == "approve" {
        format!(
            "GitHub write access approved for {}. Refresh the operation and continue.",
            decided.repository
        )
    } else if reason.is_empty() {
        format!("GitHub write access denied for {}.", decided.repository)
    } else {
        format!(
            "GitHub write access denied for {}: {reason}",
            decided.repository
        )
    };
    if let Some(channel) = crate::channels::access(&st.db, &session.id).await? {
        let author = Subject::new(SubjectKind::User, &principal.username);
        let message = channels::messages::create::Input {
            kind: "system".to_string(),
            urgency: "normal".to_string(),
            body,
            payload: json!({
                "permission_request_id": decided.id,
                "decision": decision,
                "repository": decided.repository,
            }),
            reply_to: None,
            idempotency_key: Some(format!("permission-decision:{}", decided.id)),
            // The channel and branch are passed to `append_and_deliver` directly.
            ..Default::default()
        };
        let (inserted, message) =
            append_and_deliver(st, &session.id, &channel, &author, &message).await?;
        record_channel_message_event(st, &session.id, &author, &message, inserted).await;
    }
    record_decision_event(st, &branch.id, &decided, decision, &principal.username).await?;
    Ok(view(decided))
}

pub(super) async fn approve_permission_request_operation(
    context: OperationContext,
    input: permission_operations::requests::approve::Input,
) -> ApiResult<permission_operations::requests::approve::Output> {
    apply_permission_decision(
        &context.state,
        &context.principal,
        &input.request,
        "approve",
        input.reason.trim(),
    )
    .await
}

pub(super) async fn deny_permission_request_operation(
    context: OperationContext,
    input: permission_operations::requests::deny::Input,
) -> ApiResult<permission_operations::requests::deny::Output> {
    apply_permission_decision(
        &context.state,
        &context.principal,
        &input.request,
        "deny",
        input.reason.trim(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_filter_is_closed() {
        assert_eq!(validate_state(Some("pending")).unwrap(), Some("pending"));
        assert!(validate_state(Some("waiting")).is_err());
    }
}
