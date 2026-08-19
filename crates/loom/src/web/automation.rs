use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use weaver_api::operations::runs as run_operations;
use weaver_api::{
    AutomationTokenReq, AutomationTokenView, FederateReq, FederationReq, FederationView, RunReq,
    RunView, SlackThreadRef,
};

use crate::auth::{Grant, Principal};

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

/// The `runs` bundle: automation-triggered session launches (GitHub Actions,
/// ops scripts, Grafana alerts). Federation/token minting (`federate`,
/// `mint_automation_token`, `list_federations`, `add_federation`,
/// `remove_federation`) below are a different bundle and are untouched here.
///
/// The legacy `/runs`, `/runs/{id}` axum routes in `web/mod.rs` still point at
/// `list_runs`/`get_run`/`create_run` below (unchanged) — route deletion is a
/// coordinated pass, not this agent's to make — so the operation-registry
/// handlers below are named `*_operation` and share their core logic with the
/// legacy handlers rather than replacing them in place.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<run_operations::list::List, _, _>(list_runs_operation),
        register::<run_operations::get::Get, _, _>(get_run_operation),
        register::<run_operations::create::Create, _, _>(create_run_operation),
    ]
}

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

/// The requesting identity's subject and allowed profile set, for the legacy
/// `/runs` route: `Grant::Admin`/`Grant::User` both act as themselves, naming
/// any profile; `Grant::Automation` is restricted to its own minted profile
/// set; nothing else may create a run.
fn run_identity(
    principal: &Principal,
    requested_profile: &str,
) -> ApiResult<(String, Vec<String>)> {
    match &principal.grant {
        Grant::Admin | Grant::User => Ok((
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
        Grant::Anonymous | Grant::Session { .. } => Err(AppError::new(
            StatusCode::FORBIDDEN,
            "session credentials cannot create automation runs",
        )),
    }
}

/// The requesting identity for `runs.create`, `actor = Internal`:
/// `authorize()` has already refused anything but `Grant::Admin` or
/// `Grant::Automation` by the time this runs (`Grant::User`/`Grant::Session`
/// cannot reach here — unlike the legacy `/runs` route above, which still
/// treats `Grant::User` the same as `Grant::Admin`; see the port report). The
/// automation grant's own profile allowlist is per-token business state, not
/// something the central actor/scope check can see, so it stays.
fn create_run_identity(
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
        Grant::Anonymous | Grant::User | Grant::Session { .. } => unreachable!(
            "authorize() only admits Admin/Automation grants to an actor = Internal operation"
        ),
    }
}

async fn run_view(st: &AppState, id: &str) -> ApiResult<RunView> {
    let run = crate::runs::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("automation run"))?;
    Ok(run.into())
}

enum LaunchFailure {
    Final,
    Retryable,
}

/// Tear down a session that finished provisioning after its automation
/// reservation was archived or removed. Cancellation wins: a late response may
/// not resurrect the run or leave its worktree/supervisor detached from the
/// operator-visible lifecycle record.
async fn remove_late_session(st: &AppState, session_id: &str) {
    let Ok(Some((session, branch))) = crate::session::with_branch(&st.db, session_id).await else {
        return;
    };
    match super::sessions::remove(st, &session, &branch, false).await {
        Ok(warnings) if !warnings.is_empty() => tracing::warn!(
            session = session_id,
            warnings = warnings.len(),
            "late cancelled automation session removed with warnings"
        ),
        Ok(_) => tracing::info!(
            session = session_id,
            "removed session that completed after automation cancellation"
        ),
        Err(error) => tracing::warn!(
            session = session_id,
            error = %error.message(),
            "could not remove session that completed after automation cancellation"
        ),
    }
}

/// Point the delivery's Slack thread at the branch the run landed on, so that
/// session can reply there and a mention in that thread reaches it. Best-effort:
/// the run itself already succeeded, and a lost route degrades to a thread the
/// operator has to answer from the dashboard instead.
async fn route_slack_thread(
    st: &AppState,
    target: Option<&SlackThreadRef>,
    branch_id: &str,
    source: &str,
) {
    let Some(target) = target else {
        return;
    };
    let Ok((channel, thread_ts)) = crate::slack::parse_thread_ref(target) else {
        return;
    };
    if let Err(error) =
        crate::slack_routes::record(&st.db, &channel, &thread_ts, branch_id, source).await
    {
        tracing::warn!(
            branch = branch_id,
            channel = %channel,
            %error,
            "could not route the delivery's Slack thread to its session"
        );
    }
}

async fn launch_run(
    st: &AppState,
    req: RunReq,
    subject: String,
    profiles: Vec<String>,
    run: crate::runs::Run,
    failure: LaunchFailure,
) -> ApiResult<RunView> {
    let actor = crate::provision::Actor::automation(
        req.source.clone(),
        subject,
        profiles,
        run.id.clone(),
        run.session_id.clone(),
    );
    let slack = req.slack.clone();
    let source = req.source.clone();
    match crate::provision::create(st.clone(), req.session, actor).await {
        Ok(created) => {
            if crate::runs::launched(&st.db, &run.id, &created.session.id).await? {
                route_slack_thread(st, slack.as_ref(), &created.branch.id, &source).await;
                run_view(st, &run.id).await
            } else {
                remove_late_session(st, &created.session.id).await;
                Err(AppError::conflict(
                    "automation launch was archived or removed while provisioning",
                ))
            }
        }
        Err(error) => {
            let still_owned = match failure {
                LaunchFailure::Final => {
                    match crate::runs::failed(&st.db, &run.id, &format!("{error:?}")).await {
                        Ok(owned) => owned,
                        Err(record_error) => {
                            tracing::warn!(
                                run = %run.id,
                                error = %record_error,
                                "could not record automation launch failure"
                            );
                            true
                        }
                    }
                }
                LaunchFailure::Retryable => match crate::runs::waiting(&st.db, &run.id).await {
                    Ok(owned) => owned,
                    Err(record_error) => {
                        tracing::warn!(
                            run = %run.id,
                            error = %record_error,
                            "could not return automation launch to waiting"
                        );
                        true
                    }
                },
            };
            if !still_owned {
                remove_late_session(st, &run.session_id).await;
            }
            Err(super::provision_error(error))
        }
    }
}

async fn prompt_channel_run(
    st: &AppState,
    req: &RunReq,
    run: crate::runs::Run,
) -> ApiResult<RunView> {
    let Some(session) = crate::session::get(&st.db, &run.session_id)
        .await?
        .filter(|session| session.status == "running" && session.protocol == "acp")
    else {
        crate::runs::waiting(&st.db, &run.id).await.ok();
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "automation channel session is not ready; retry this delivery",
        ));
    };
    let Some(handle) = st.acp.get(&session.id) else {
        crate::runs::waiting(&st.db, &run.id).await.ok();
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "automation channel session is being adopted; retry this delivery",
        ));
    };
    let channel = run
        .channel
        .as_deref()
        .expect("channel dispatch requires a channel");
    let by = format!("automation:{}/{channel}", run.service_tag);
    let goal = req
        .session
        .goal
        .clone()
        .expect("channel runs require a goal");
    if let Err(error) = handle
        .stop_and_send(goal.clone(), Some(by.clone()), Vec::new())
        .await
    {
        crate::runs::waiting(&st.db, &run.id).await.ok();
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("automation channel rejected the update: {error}"),
        ));
    }
    crate::events::record(
        &st.db,
        &st.bus,
        &session.branch_id,
        "nudge",
        serde_json::json!({ "by": by, "text": goal }),
    )
    .await
    .ok();
    if !crate::runs::launched(&st.db, &run.id, &session.id).await? {
        return Err(AppError::conflict(
            "automation delivery was archived or removed while running",
        ));
    }
    // Each alert on a channel arrives in its own thread while the session stays
    // the same, so routes accumulate on this one branch. The session's `slack`
    // wiring tag is deliberately left alone: one status card cannot follow a
    // session that is triaging several incidents at once.
    route_slack_thread(st, req.slack.as_ref(), &session.branch_id, &run.source).await;
    run_view(st, &run.id).await
}

async fn dispatch_channel_run(
    st: &AppState,
    subject: String,
    profiles: Vec<String>,
    run: crate::runs::Run,
) -> ApiResult<RunView> {
    let req: RunReq = serde_json::from_str(&run.request_json)?;
    match crate::runs::route_channel(&st.db, &run.id).await? {
        crate::runs::ChannelAction::Launch(run) => {
            launch_run(st, req, subject, profiles, run, LaunchFailure::Retryable).await
        }
        crate::runs::ChannelAction::Prompt(run) => prompt_channel_run(st, &req, run).await,
        crate::runs::ChannelAction::Ready(run) => Ok(run.into()),
        crate::runs::ChannelAction::Busy(_) => Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "automation channel is provisioning or orphaned; retry this delivery",
        )),
    }
}

/// Shared body of `POST /runs` (legacy) and `runs.create` (operation
/// registry): everything after identity resolution, which is the one piece
/// that differs between the two routes (see `run_identity` vs
/// `create_run_identity`).
async fn create_run_core(
    st: &AppState,
    principal: &Principal,
    mut req: RunReq,
    subject: String,
    profiles: Vec<String>,
) -> ApiResult<RunView> {
    let profile = req.profile.trim().to_string();
    if !matches!(req.source.as_str(), "actions" | "ops" | "grafana") {
        return Err(AppError::bad_request(
            "run source must be 'actions', 'ops', or 'grafana'",
        ));
    }
    if let Some(watch_id) = req.watch_id.as_deref() {
        if weaver_core::watch::get(&st.db, watch_id).await?.is_none() {
            return Err(AppError::bad_request(format!("unknown watch '{watch_id}'")));
        }
    }
    req.session.profile = Some(profile.clone());
    req.session.class = None;
    // Reject a malformed thread up front rather than accepting the run and
    // silently dropping the route: the caller can then fix and redeliver.
    if let Some(target) = req.slack.as_ref() {
        crate::slack::parse_thread_ref(target).map_err(AppError::bad_request)?;
    }
    req.channel = match req.channel.take() {
        Some(channel) => {
            let channel = channel.trim().to_string();
            crate::runs::validate_channel(&channel)
                .map_err(|error| AppError::bad_request(error.to_string()))?;
            if req
                .session
                .goal
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                return Err(AppError::bad_request(
                    "channel automation runs require a non-empty session goal",
                ));
            }
            let launch_profile = crate::profile::get(&st.db, &profile)
                .await?
                .ok_or_else(|| AppError::bad_request(format!("unknown profile '{profile}'")))?;
            if launch_profile.protocol != "acp" {
                return Err(AppError::bad_request(
                    "automation channels require an ACP profile",
                ));
            }
            Some(channel)
        }
        None => None,
    };

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
        crate::runs::NewRun {
            subject: &subject,
            source: &req.source,
            service_tag,
            profile: &profile,
            idempotency_key: &idempotency_key,
            channel: req.channel.as_deref(),
            request_json: &request_json,
        },
    )
    .await?;
    if req.channel.is_some() {
        let run = match reservation {
            crate::runs::Reservation::Existing(run)
                if run.channel.as_deref() != req.channel.as_deref() =>
            {
                return Ok(run.into());
            }
            crate::runs::Reservation::Existing(run)
                if matches!(
                    run.status.as_str(),
                    "running" | "failed" | "cancelled" | "completed"
                ) =>
            {
                return Ok(run.into());
            }
            crate::runs::Reservation::Existing(run) if run.status == "delivering" => {
                if !crate::runs::claim_stale_delivery(&st.db, &run.id).await? {
                    return Err(AppError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "automation channel delivery is in progress; retry this delivery",
                    ));
                }
                crate::runs::get(&st.db, &run.id)
                    .await?
                    .ok_or_else(|| AppError::not_found("automation run"))?
            }
            crate::runs::Reservation::Existing(run) | crate::runs::Reservation::Created(run) => run,
        };
        return dispatch_channel_run(st, subject, profiles, run).await;
    }
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
                return Ok(run.into());
            }
            if !crate::runs::claim_stale(&st.db, &run.id).await? {
                return Ok(run.into());
            }
            run
        }
        crate::runs::Reservation::Created(run) => run,
    };
    launch_run(st, req, subject, profiles, run, LaunchFailure::Final).await
}

/// `POST /runs` — the legacy axum route, unchanged, still reachable by
/// `Grant::User` per `web/auth.rs`'s `user_grant_allows`. Kept in place
/// alongside `create_run_operation` until the coordinator's route-deletion
/// pass; see `bound_operations`.
pub(super) async fn create_run(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<RunReq>,
) -> ApiResult<Json<RunView>> {
    let profile = req.profile.trim().to_string();
    let (subject, profiles) = run_identity(&principal, &profile)?;
    Ok(Json(
        create_run_core(&st, &principal, req, subject, profiles).await?,
    ))
}

/// `runs.create` — the operation-registry handler. `actor = Internal` means
/// `authorize()` has already narrowed the reachable grants to
/// `Admin`/`Automation` (see `create_run_identity`) before this runs.
pub(super) async fn create_run_operation(
    context: OperationContext,
    input: run_operations::create::Input,
) -> ApiResult<RunView> {
    let req = RunReq {
        profile: input.profile,
        idempotency_key: input.idempotency_key,
        source: input.source,
        watch_id: input.watch_id,
        channel: input.channel,
        slack: input.slack,
        session: input.session,
    };
    let profile = req.profile.trim().to_string();
    let (subject, profiles) = create_run_identity(&context.principal, &profile)?;
    create_run_core(&context.state, &context.principal, req, subject, profiles).await
}

/// `GET /runs` — the legacy axum route, unchanged. Still reachable by
/// `Grant::Automation`, filtered to that credential's own subject; see
/// `list_runs_operation` for why the new operation cannot preserve that.
pub(super) async fn list_runs(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<Vec<RunView>>> {
    let subject = match &principal.grant {
        Grant::Admin | Grant::User => None,
        Grant::Automation { subject, .. } => Some(subject.as_str()),
        Grant::Anonymous | Grant::Session { .. } => {
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

/// `runs.list` is declared `actor = User`: only `Grant::Admin`/`Grant::User`
/// ever reach this handler (`Grant::Automation` and `Grant::Session` are
/// refused by `authorize()` before the body runs, unlike the legacy route
/// above), so unlike `list_runs` there is no subject to filter by — this is a
/// narrowing versus the legacy route worth flagging; see the port report.
pub(super) async fn list_runs_operation(
    context: OperationContext,
    _input: run_operations::list::Input,
) -> ApiResult<Vec<RunView>> {
    Ok(crate::runs::list_for(&context.state.db, None)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// `GET /runs/{id}` — the legacy axum route, unchanged. Still reachable by
/// `Grant::Automation`, gated to runs it owns.
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

/// `runs.get` is declared `actor = User`, same reasoning as
/// `list_runs_operation`: only `Grant::Admin`/`Grant::User` reach here, so
/// the legacy handler's `Grant::Automation`-subject-match check above is
/// unreachable through this path and dropped.
pub(super) async fn get_run_operation(
    context: OperationContext,
    input: run_operations::get::Input,
) -> ApiResult<RunView> {
    let run = crate::runs::get(&context.state.db, &input.id)
        .await?
        .ok_or_else(|| AppError::not_found("automation run"))?;
    Ok(run.into())
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
