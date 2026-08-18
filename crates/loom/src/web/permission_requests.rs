//! Human approval workflow for session-scoped external access expansions.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::{
    CreateChannelMessageReq, CreatePermissionRequestReq, DecidePermissionRequestReq,
    EffectivePermissionsView, PermissionRequestView,
};
use weaver_core::{branch as branch_mod, tags};

use crate::{
    auth::Principal,
    channels::{MessageKind, NewMessage, Subject, SubjectKind, Urgency},
    permission_requests,
};

use super::{
    channels::{append_and_deliver, record_channel_message_event},
    effective_repositories, require_session, validate_github_write, ApiResult, AppError, AppState,
    OperationContext,
};

#[derive(Debug, Default, Deserialize)]
pub(super) struct PermissionRequestQuery {
    state: Option<String>,
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

fn require_human(principal: &Principal) -> ApiResult<()> {
    if principal.is_human() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "a human operator must decide access requests; use `loom permissions request`",
        ))
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

pub(super) async fn effective_permissions_operation(
    context: OperationContext,
    input: permission_operations::SessionInput,
) -> ApiResult<EffectivePermissionsView> {
    let st = context.state;
    let key = input.session;
    let (session, _) = require_session(&st.db, &key).await?;
    let github_repositories = effective_repositories(&st.db, &session)
        .await
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
                    .capabilities
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
        pending_requests,
    })
}

pub(super) async fn effective_permissions(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
) -> ApiResult<Json<EffectivePermissionsView>> {
    effective_permissions_operation(
        OperationContext::new(st, principal),
        permission_operations::SessionInput { session: key },
    )
    .await
    .map(Json)
}

pub(super) async fn list_permission_requests_operation(
    context: OperationContext,
    input: permission_operations::ListRequestsInput,
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

pub(super) async fn list_permission_requests(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
    Query(query): Query<PermissionRequestQuery>,
) -> ApiResult<Json<Vec<PermissionRequestView>>> {
    list_permission_requests_operation(
        OperationContext::new(st, principal),
        permission_operations::ListRequestsInput {
            session: key,
            state: query.state,
        },
    )
    .await
    .map(Json)
}

pub(super) async fn create_permission_request_operation(
    context: OperationContext,
    input: permission_operations::CreateRequestInput,
) -> ApiResult<PermissionRequestView> {
    let st = context.state;
    let principal = context.principal;
    let key = input.session;
    let req = input.request;
    let (session, branch) = require_session(&st.db, &key).await?;
    if req.kind.trim() != permission_requests::GITHUB_REPOSITORY_KIND {
        return Err(AppError::bad_request("kind must be 'github_repository'"));
    }
    if req.mode.trim() != "write" {
        return Err(AppError::bad_request("mode must be 'write'"));
    }
    let repository = crate::repo::parse_slug(req.repository.trim())
        .map_err(AppError::bad_request)?
        .slug();
    let reason = req.reason.trim();
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

pub(super) async fn create_permission_request(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreatePermissionRequestReq>,
) -> ApiResult<(StatusCode, Json<PermissionRequestView>)> {
    create_permission_request_operation(
        OperationContext::new(st, principal),
        permission_operations::CreateRequestInput {
            session: key,
            request: req,
        },
    )
    .await
    .map(|value| (StatusCode::CREATED, Json(value)))
}

pub(super) async fn decide_permission_request(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<DecidePermissionRequestReq>,
) -> ApiResult<Json<PermissionRequestView>> {
    require_human(&principal)?;
    let request = permission_requests::get(&st.db, &id)
        .await?
        .ok_or_else(|| AppError::not_found("permission request"))?;
    if request.state != permission_requests::PermissionRequestState::Pending {
        return Err(AppError::conflict("permission request is already resolved"));
    }
    let (session, branch) = require_session(&st.db, &request.session_id).await?;
    let decision = req.decision.trim().to_ascii_lowercase();
    let reason = req.reason.trim();
    let changed = match decision.as_str() {
        "approve" => {
            validate_github_write(&st, &session, &request.repository).await?;
            permission_requests::approve_github(&st.db, &id, &principal.username, reason).await?
        }
        "deny" => permission_requests::deny(&st.db, &id, &principal.username, reason).await?,
        _ => return Err(AppError::bad_request("decision must be approve or deny")),
    };
    if !changed {
        return Err(AppError::conflict("permission request is already resolved"));
    }
    let decided = permission_requests::get(&st.db, &id)
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
        let message = CreateChannelMessageReq {
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
        };
        let (inserted, message) =
            append_and_deliver(&st, &session.id, &channel, &author, &message).await?;
        record_channel_message_event(&st, &session.id, &author, &message, inserted).await;
    }
    crate::events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "permission_decision",
        json!({
            "request_id": decided.id,
            "decision": decision,
            "repository": decided.repository,
            "by": principal.username,
        }),
    )
    .await?;
    Ok(Json(view(decided)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthVia, Grant};

    fn principal(grant: Grant) -> Principal {
        Principal {
            username: "alice".to_string(),
            github_login: None,
            via: AuthVia::Token,
            grant,
            automation_context: None,
        }
    }

    #[test]
    fn only_humans_can_decide_requests() {
        assert!(require_human(&principal(Grant::User)).is_ok());
        assert!(require_human(&principal(Grant::Session {
            session_id: "s".to_string(),
            branch_id: "b".to_string(),
            capabilities: None,
        }))
        .is_err());
    }

    #[test]
    fn state_filter_is_closed() {
        assert_eq!(validate_state(Some("pending")).unwrap(), Some("pending"));
        assert!(validate_state(Some("waiting")).is_err());
    }
}
