use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use weaver_api::{
    AutomationTokenReq, AutomationTokenView, FederateReq, FederationReq, FederationView, RunReq,
    RunView,
};

use crate::auth::{Grant, Principal};

use super::{ApiResult, AppError, AppState};

fn github_idempotency_key(
    context: &crate::automation::GithubContext,
    requested: &str,
) -> ApiResult<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Ok(format!(
            "github-run:{}:{}:{}",
            context.repository_id, context.run_id, context.run_attempt
        ));
    }
    if requested.len() > 128
        || !requested
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(AppError::bad_request(
            "GitHub idempotency_key must be 1-128 ASCII letters, digits, '.', '_', ':', or '-'",
        ));
    }
    Ok(format!(
        "github-caller:{}:{}",
        context.repository_id, requested
    ))
}

pub(super) async fn federate(
    State(st): State<AppState>,
    Json(req): Json<FederateReq>,
) -> ApiResult<Json<AutomationTokenView>> {
    let token = crate::automation::federate(&st.db, &req.token)
        .await
        .map_err(|error| AppError::new(StatusCode::UNAUTHORIZED, error.to_string()))?;
    Ok(Json(token))
}

pub(super) async fn mint_automation_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<AutomationTokenReq>,
) -> ApiResult<Json<AutomationTokenView>> {
    if !principal.is_admin() {
        return Err(AppError::new(StatusCode::FORBIDDEN, "admin grant required"));
    }
    Ok(Json(
        crate::automation::mint(&st.db, &req.subject, req.profiles, req.ttl_secs, None)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?,
    ))
}

pub(super) async fn list_federations(
    State(st): State<AppState>,
) -> ApiResult<Json<Vec<FederationView>>> {
    Ok(Json(crate::automation::federation_list(&st.db).await?))
}

pub(super) async fn add_federation(
    State(st): State<AppState>,
    Json(req): Json<FederationReq>,
) -> ApiResult<(StatusCode, Json<FederationView>)> {
    let mapping = crate::automation::federation_add(&st.db, &req)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    Ok((StatusCode::CREATED, Json(mapping)))
}

pub(super) async fn remove_federation(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if crate::automation::federation_remove(&st.db, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("federation mapping"))
    }
}

fn run_identity(
    principal: &Principal,
    requested_profile: &str,
) -> ApiResult<(String, Vec<String>)> {
    match &principal.grant {
        Grant::Admin => Ok((
            principal.username.clone(),
            vec![requested_profile.to_string()],
        )),
        Grant::Automation { subject, profiles } => {
            if !profiles.iter().any(|profile| profile == requested_profile) {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    format!("automation grant does not allow profile '{requested_profile}'"),
                ));
            }
            Ok((subject.clone(), profiles.clone()))
        }
        Grant::Session { .. } => Err(AppError::new(
            StatusCode::FORBIDDEN,
            "session credentials cannot create automation runs",
        )),
    }
}

pub(super) async fn create_run(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(mut req): Json<RunReq>,
) -> ApiResult<Json<RunView>> {
    let profile = req.profile.trim().to_string();
    let (subject, profiles) = run_identity(&principal, &profile)?;
    if !matches!(req.source.as_str(), "actions" | "ops" | "grafana") {
        return Err(AppError::bad_request(
            "run source must be 'actions', 'ops', or 'grafana'",
        ));
    }
    req.session.profile = Some(profile.clone());
    req.session.class = None;

    let idempotency_key = match &principal.automation_context {
        Some(context) if context.provider == "github" => {
            let context = context.github.as_ref().ok_or_else(|| {
                AppError::new(
                    StatusCode::UNAUTHORIZED,
                    "GitHub automation credential is missing workflow context",
                )
            })?;
            if let Some(repo) = req
                .session
                .repo
                .as_deref()
                .filter(|repo| !repo.trim().is_empty())
            {
                if repo.trim().trim_end_matches(".git") != context.repository {
                    return Err(AppError::new(
                        StatusCode::FORBIDDEN,
                        "run repository does not match the verified workflow repository",
                    ));
                }
            }
            req.session.repo = Some(context.repository.clone());
            github_idempotency_key(context, &req.idempotency_key)?
        }
        _ => {
            let key = req.idempotency_key.trim();
            if key.is_empty() {
                return Err(AppError::bad_request("idempotency_key is required"));
            }
            key.to_string()
        }
    };
    let request_json = serde_json::to_string(&req)?;
    let service_tag = principal
        .automation_context
        .as_ref()
        .map(|context| context.service_tag.as_str())
        .unwrap_or(req.source.as_str());
    let reservation = crate::runs::reserve(
        &st.db,
        &subject,
        &req.source,
        service_tag,
        &profile,
        &idempotency_key,
        &request_json,
    )
    .await?;
    let run = match reservation {
        crate::runs::Reservation::Existing(run) => {
            if let Some(session) = crate::session::get(&st.db, &run.session_id).await? {
                // A failed launch deliberately leaves a recoverable session
                // record. Idempotent delivery must return that failed run, not
                // relabel it as running merely because the record exists.
                if !matches!(session.status.as_str(), "done" | "error" | "archived") {
                    crate::runs::launched(&st.db, &run.id, &run.session_id).await?;
                }
                let run = crate::runs::get(&st.db, &run.id)
                    .await?
                    .ok_or_else(|| AppError::not_found("automation run"))?;
                return Ok(Json(run.into()));
            }
            if !crate::runs::claim_stale(&st.db, &run.id).await? {
                return Ok(Json(run.into()));
            }
            run
        }
        crate::runs::Reservation::Created(run) => run,
    };
    let actor = crate::runtime::Actor::automation(
        req.source.clone(),
        subject,
        profiles,
        run.id.clone(),
        run.session_id.clone(),
    );
    match crate::runtime::create_session(st.clone(), req.session, actor).await {
        Ok(session) => {
            crate::runs::launched(&st.db, &run.id, &session.id).await?;
            let run = crate::runs::get(&st.db, &run.id)
                .await?
                .ok_or_else(|| AppError::not_found("automation run"))?;
            Ok(Json(run.into()))
        }
        Err(error) => {
            crate::runs::failed(&st.db, &run.id, &format!("{error:?}"))
                .await
                .ok();
            Err(error)
        }
    }
}

pub(super) async fn list_runs(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<Vec<RunView>>> {
    let subject = match &principal.grant {
        Grant::Admin => None,
        Grant::Automation { subject, .. } => Some(subject.as_str()),
        Grant::Session { .. } => {
            return Err(AppError::new(StatusCode::FORBIDDEN, "run access forbidden"))
        }
    };
    Ok(Json(
        crate::runs::list_for(&st.db, subject)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

pub(super) async fn get_run(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<RunView>> {
    let run = crate::runs::get(&st.db, &id)
        .await?
        .ok_or_else(|| AppError::not_found("automation run"))?;
    if matches!(&principal.grant, Grant::Automation { subject, .. } if subject != &run.actor_subject)
    {
        return Err(AppError::new(StatusCode::FORBIDDEN, "run access forbidden"));
    }
    Ok(Json(run.into()))
}

#[cfg(test)]
mod tests {
    use super::github_idempotency_key;
    use crate::automation::GithubContext;

    fn context() -> GithubContext {
        GithubContext {
            repository_id: "1234".to_string(),
            run_id: "55".to_string(),
            run_attempt: "2".to_string(),
            ..GithubContext::default()
        }
    }

    #[test]
    fn github_caller_can_choose_a_deterministic_idempotency_key() {
        assert_eq!(
            github_idempotency_key(&context(), "prose-cleanup:issue:7:abc123").unwrap(),
            "github-caller:1234:prose-cleanup:issue:7:abc123"
        );
        assert_eq!(
            github_idempotency_key(&context(), "").unwrap(),
            "github-run:1234:55:2"
        );
    }

    #[test]
    fn github_caller_idempotency_keys_are_bounded_and_log_safe() {
        assert!(github_idempotency_key(&context(), "contains spaces").is_err());
        assert!(github_idempotency_key(&context(), &"x".repeat(129)).is_err());
    }
}
