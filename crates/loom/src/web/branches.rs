use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use weaver_api::operations::branches as ops;
use weaver_api::operations::slack as slack_operations;
use weaver_api::{BranchStatusReq, BranchView, CreateChannelMessageReq, CreateEventReq, TagReq};
use weaver_core::branch::{TitleProvenance, TitleUpdate};
use weaver_core::{branch as branch_mod, config, tags};

use crate::{events, session as session_mod};

use super::operations::{register, Bound, OperationContext};
use super::sessions::ByQuery;
use super::{author_or_manual, branch_view, require_branch};
use super::{ApiResult, AppError, AppState};
use axum::http::StatusCode;

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

pub(super) async fn list_branches(State(st): State<AppState>) -> ApiResult<Json<Vec<BranchView>>> {
    let branches = branch_mod::list(&st.db).await?;
    let mut out: Vec<BranchView> = Vec::with_capacity(branches.len());
    for b in branches {
        out.push(branch_view(&st.db, &b).await?);
    }
    Ok(Json(out))
}

pub(super) async fn get_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    Ok(Json(branch_view(&st.db, &branch).await?))
}

#[derive(Debug, Deserialize)]
pub(super) struct PatchBranchReq {
    title: Option<String>,
    expected_title: Option<String>,
    expected_title_provenance: Option<String>,
    goal: Option<String>,
    description: Option<String>,
}

pub(super) async fn patch_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchBranchReq>,
) -> ApiResult<Json<BranchView>> {
    let initial_branch = require_branch(&st.db, &key).await?;
    let goal_sessions = if req.goal.is_some() {
        session_mod::handoff_capable_for_branch(&st.db, &initial_branch.id).await?
    } else {
        Vec::new()
    };
    let initial_goal_ids: std::collections::BTreeSet<_> = goal_sessions
        .iter()
        .map(|session| session.id.clone())
        .collect();
    let _goal_permits = st
        .launch_gate
        .acquire_sessions(goal_sessions.iter().map(|session| session.id.as_str()))
        .await;
    let branch = require_branch(&st.db, &initial_branch.id).await?;
    let current_goal_sessions = if req.goal.is_some() {
        session_mod::handoff_capable_for_branch(&st.db, &branch.id).await?
    } else {
        Vec::new()
    };
    if current_goal_sessions
        .iter()
        .any(|session| !initial_goal_ids.contains(&session.id))
    {
        return Err(AppError::conflict(
            "the branch's handoff-capable session changed while the goal edit was waiting; retry",
        ));
    }
    if let Some(title) = &req.title {
        let title = branch_mod::sanitize_user_title(title)
            .ok_or_else(|| AppError::bad_request("title must not be empty"))?;
        let expected_title = req.expected_title.as_deref().ok_or_else(|| {
            AppError::bad_request("expected_title is required when renaming a branch")
        })?;
        let expected_provenance = req
            .expected_title_provenance
            .as_deref()
            .ok_or_else(|| {
                AppError::bad_request(
                    "expected_title_provenance is required when renaming a branch",
                )
            })?
            .parse::<TitleProvenance>()
            .map_err(AppError::bad_request)?;
        match branch_mod::replace_title(
            &st.db,
            &branch.id,
            expected_title,
            expected_provenance,
            &title,
            TitleProvenance::User,
        )
        .await?
        {
            TitleUpdate::Applied(_) => {
                crate::channels::update_branch_channel_names(&st.db, &branch.id, &title).await?;
            }
            TitleUpdate::Stale(current) => {
                return Err(AppError::conflict(
                    "the task label changed while it was being edited; review it and retry",
                )
                .with_fields(json!({ "branch": super::branch_view(&st.db, &current).await? })));
            }
            TitleUpdate::Missing => return Err(AppError::not_found("branch")),
        }
    }
    if let Some(goal) = &req.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal, "user").await?;
        for session in &current_goal_sessions {
            session_mod::bump_mutation_revision(&st.db, &session.id).await?;
            crate::channels::update_session_goal(&st.db, &session.id, goal).await?;
        }
    }
    if let Some(description) = &req.description {
        branch_mod::set_description(&st.db, &branch.id, description).await?;
    }
    if req.title.is_some() || req.goal.is_some() || req.description.is_some() {
        tracing::info!(
            branch = %branch.id,
            title = req.title.is_some(),
            goal = req.goal.is_some(),
            description = req.description.is_some(),
            "branch patched"
        );
    }
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    Ok(Json(branch_view(&st.db, &branch).await?))
}

/// The attention value that means "calm" — never stored (absence is calm);
/// both the input that clears the tag and the value `loom status` reads
/// back as the default.
const CALM_STATUS: &str = "ok";

/// Set the agent's attention level and current-state message in one call:
/// validate the level, write the description when a message is given,
/// set-or-clear the `attention` tag, and record exactly one `tag` event —
/// what `loom status set --tag <level> [--message <message>]` does against the local
/// database in one process, reproduced server-side so a networked CLI gets
/// the same one-event, effectively-atomic semantics.
pub(super) async fn set_branch_status(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<BranchStatusReq>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    let level = req.level.trim().to_ascii_lowercase();
    if level != CALM_STATUS && !tags::is_valid_value(tags::ATTENTION_KEY, &level) {
        return Err(AppError::bad_request(format!(
            "unknown status '{level}' — expected one of {CALM_STATUS}, {}",
            tags::ATTENTION_VALUES.join(", ")
        )));
    }
    let message = req
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    if let Some(message) = message {
        branch_mod::set_description(&st.db, &branch.id, message).await?;
    }
    let value = if level == CALM_STATUS {
        tags::clear(&st.db, &branch.id, tags::ATTENTION_KEY).await?;
        String::new()
    } else {
        tags::set(&st.db, &branch.id, tags::ATTENTION_KEY, &level, "", "agent").await?;
        level.clone()
    };
    tracing::info!(branch = %branch.id, level = %level, "branch status set");
    // The message rides the event as its note, so the event log carries the
    // full trail of status reports — the progress log the activity feed and a
    // wired GitHub thread render — not just the level transitions.
    // Propagated, not swallowed: the event log is the source of truth for the
    // status trail (the activity feed and a wired GitHub card render from it),
    // so a dropped insert must fail the write loudly enough to be retried.
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "tag",
        json!({
            "key": tags::ATTENTION_KEY,
            "value": value,
            "note": message.unwrap_or_default(),
            "by": "agent",
        }),
    )
    .await?;
    // Preserve tags and external mirrors during the migration, while also
    // making status a typed item in the session's durable communication log.
    if let Some(channel_id) =
        crate::channels::session_channel_for_branch(&st.db, &branch.id).await?
    {
        let urgency = crate::channels::Urgency::from_status_level(&level);
        let author =
            crate::channels::Subject::new(crate::channels::SubjectKind::Session, &channel_id);
        crate::channels::append(
            &st.db,
            &channel_id,
            crate::channels::NewMessage {
                kind: crate::channels::MessageKind::Status,
                urgency,
                author: &author,
                body: message.unwrap_or(&level),
                payload: &json!({ "level": level }),
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await?;
    }
    // Mirror the trail onto every origin thread the branch is wired to — the
    // GitHub comment and/or the Slack message (each a no-op when its wiring tag
    // is absent) — detached, so an integration hiccup never slows or fails the
    // status write.
    crate::slack::spawn_status_mirrors(st.clone(), branch.id.clone());
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    Ok(Json(branch_view(&st.db, &branch).await?))
}

/// `POST /api/branches/{id}/slack/reply` — post a message from the session back
/// to a Slack thread it owns. The token stays on the server: the agent (holding
/// `LOOM_TOKEN`) calls this route rather than being handed the workspace-wide bot
/// token. The Slack analog of the GitHub-triggered session replying with `gh`.
///
/// Two destinations, both resolved server-side. Without `thread`, the branch's
/// `slack` wiring tag — the conversation the session was born from. With
/// `thread`, one of the threads an automation delivery routed to this branch
/// ([`crate::slack_routes`]), which is how one operator session answers in each
/// alert's own thread. A thread never delivered to this branch is refused, so the
/// field selects among the session's own threads rather than granting the
/// workspace.
#[derive(Deserialize)]
pub(super) struct SlackReplyReq {
    pub text: String,
    #[serde(default)]
    pub thread: Option<weaver_api::SlackThreadRef>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

pub(super) async fn slack_reply(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<SlackReplyReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let branch = require_branch(&st.db, &key).await?;
    let text = req.text.trim();
    if text.is_empty() {
        return Err(AppError::bad_request("text is required"));
    }
    if req.thread.is_none() {
        // Preserve the compatibility route while making the session channel the
        // canonical reply record and delivery bus. The branch wiring is checked
        // before appending, matching this endpoint's historical failure shape.
        let wired = tags::get(&st.db, &branch.id, crate::slack::WIRED_TAG)
            .await?
            .ok_or_else(|| AppError::bad_request("this branch is not wired to a Slack thread"))?;
        if crate::slack::parse_wiring(&wired.value).is_none() {
            return Err(AppError::bad_request("malformed slack wiring tag"));
        }
        let channel_id = crate::channels::session_channel_for_branch(&st.db, &branch.id)
            .await?
            .ok_or_else(|| AppError::bad_request("this branch has no open session channel"))?;
        let channel = crate::channels::access(&st.db, &channel_id)
            .await?
            .ok_or_else(|| AppError::not_found("channel"))?;
        let author =
            crate::channels::Subject::new(crate::channels::SubjectKind::Session, &channel_id);
        let request = CreateChannelMessageReq {
            kind: "result".to_string(),
            urgency: "normal".to_string(),
            body: text.to_string(),
            payload: json!({ "compatibility_source": "slack_reply" }),
            reply_to: None,
            idempotency_key: req.idempotency_key,
        };
        let (inserted, message) =
            super::channels::append_and_deliver(&st, &channel_id, &channel, &author, &request)
                .await?;
        super::channels::record_channel_message_event(
            &st,
            &channel_id,
            &author,
            &message,
            inserted,
        )
        .await;
        let delivery = message
            .deliveries
            .iter()
            .find(|delivery| delivery.binding_id == weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID)
            .ok_or_else(|| AppError::bad_request("this branch is not wired to a Slack thread"))?;
        if delivery.state == "failed" {
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                delivery
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "Slack delivery failed".to_string()),
            ));
        }
        return Ok(Json(json!({
            "posted": delivery.state == "delivered",
            "ts": delivery.external_id,
            "message_id": message.id,
            "delivery": delivery,
        })));
    }

    let target = req
        .thread
        .as_ref()
        .ok_or_else(|| AppError::bad_request("thread is required"))?;
    let (channel, root) = crate::slack::parse_thread_ref(target).map_err(AppError::bad_request)?;
    if !crate::slack_routes::allows(&st.db, &branch.id, &channel, &root).await? {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "this session holds no Slack route for that thread",
        ));
    }
    let web = crate::slack::SlackWeb::from_db(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("Slack is not configured on this server"))?;
    let ts = web
        .post_message(&channel, Some(&root), text)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(json!({ "posted": true, "ts": ts })))
}

/// `state`'s wire name, matching `SocketState`'s own `#[serde(rename_all =
/// "snake_case")]` — reproduced by hand because `loom_deliver::slack` does
/// not derive `Deserialize`/`JsonSchema` on it, so it cannot be `weaver-api`'s
/// operation `Output` type directly (see [`slack_connection_status_view`]).
fn socket_state_str(state: crate::slack::SocketState) -> &'static str {
    match state {
        crate::slack::SocketState::Idle => "idle",
        crate::slack::SocketState::Connecting => "connecting",
        crate::slack::SocketState::Connected => "connected",
        crate::slack::SocketState::Failed => "failed",
    }
}

/// The state of every link in the Slack trigger path, for the Connections
/// settings pane. Shared by [`slack_status`] and `slack.connection_status`.
///
/// One `connected` boolean is not enough to run this integration: a deployment
/// can hold a live socket and still discard every mention — because the bot
/// token is a person's rather than the app's, because the app-level token opened
/// a different app than the bot belongs to, because no repository is set, or
/// because the access list excludes everyone. Each link is reported separately
/// so the pane can name the one that is broken instead of reporting health.
async fn slack_connection_status_view(
    st: &AppState,
) -> slack_operations::connection_status::Output {
    let app_token_set = !crate::slack::app_token(&st.db).await.is_empty();
    let bot_token_set = !crate::slack::bot_token(&st.db).await.is_empty();
    let enabled = crate::slack::is_enabled(&st.db).await;

    // `auth.test` proves the bot credential still works and says who loom is —
    // including whether the token belongs to the app or to a person.
    let identity = match crate::slack::SlackWeb::from_db(&st.db).await {
        Some(web) => match web.auth_test().await {
            Ok(id) => {
                let token_kind = if id.is_bot() { "bot" } else { "user" }.to_string();
                Some(slack_operations::connection_status::SlackIdentityView {
                    user_id: Some(id.user_id),
                    team_id: Some(id.team_id),
                    token_kind: Some(token_kind),
                    error: None,
                })
            }
            Err(e) => Some(slack_operations::connection_status::SlackIdentityView {
                user_id: None,
                team_id: None,
                token_kind: None,
                error: Some(e.to_string()),
            }),
        },
        None => None,
    };

    let access = match crate::slack::access(&st.db).await {
        crate::slack::Access::Workspace => slack_operations::connection_status::SlackAccessView {
            mode: "workspace".to_string(),
            users: Vec::new(),
        },
        crate::slack::Access::Listed(users) => {
            slack_operations::connection_status::SlackAccessView {
                mode: "listed".to_string(),
                users,
            }
        }
    };

    // What the supervisor is actually doing, as opposed to what a fresh
    // credential probe suggests it could do.
    let health = crate::slack::health();
    let socket = slack_operations::connection_status::SlackSocketView {
        state: socket_state_str(health.state).to_string(),
        app_id: health.app_id,
        connected_at: health.connected_at,
        last_error: health.last_error,
        last_event_at: health.last_event_at,
        events_received: health.events_received,
        sessions_launched: health.sessions_launched,
        followups_routed: health.followups_routed,
        last_skip: health.last_skip,
        last_skip_at: health.last_skip_at,
    };

    slack_operations::connection_status::Output {
        enabled,
        app_token_set,
        bot_token_set,
        configured: app_token_set && bot_token_set,
        identity,
        access,
        default_repo: config::get(&st.db, "slack.default_repo")
            .await
            .unwrap_or_default()
            .trim()
            .to_string(),
        socket,
    }
}

/// `GET /api/slack/status` — the legacy route `slack.connection_status` now
/// also serves.
pub(super) async fn slack_status(State(st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::to_value(
        slack_connection_status_view(&st).await,
    )?))
}

/// `slack.connection_status` — the twin of [`slack_status`].
async fn slack_connection_status_operation(
    context: OperationContext,
    _input: slack_operations::connection_status::Input,
) -> ApiResult<slack_operations::connection_status::Output> {
    Ok(slack_connection_status_view(&context.state).await)
}

/// Append a raw event row to a branch's log — the escape hatch for an event
/// kind with no dedicated mutating route of its own (namely `loom hook`,
/// which has no other server-side action to piggyback on). Publishes to the
/// bus like every other mutation, unlike the local `record_local` this
/// replaces, so SSE subscribers see it too.
pub(super) async fn create_branch_event(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<CreateEventReq>,
) -> ApiResult<Json<weaver_core::events::Event>> {
    let branch = require_branch(&st.db, &key).await?;
    let kind = req.kind.trim();
    if kind.is_empty() {
        return Err(AppError::bad_request("event kind is required"));
    }
    let event = events::record(&st.db, &st.bus, &branch.id, kind, req.data).await?;
    tracing::info!(branch = %branch.id, kind = %kind, "branch event created");
    Ok(Json(event))
}

/// Set (upsert) a tag on a branch — the branch-scoped counterpart of
/// `sessions.tags.set`, for a `loom sessions tags` target with no live session
/// (a finished session, or `--session` pointing at another branch entirely).
pub(super) async fn set_branch_tag(
    State(st): State<AppState>,
    Path((key, tag_key)): Path<(String, String)>,
    Json(req): Json<TagReq>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    let value = req.value.trim();
    if crate::github::is_reserved_tag(&tag_key) {
        return Err(AppError::bad_request(format!(
            "'{tag_key}' is loom-internal bookkeeping — it can be cleared, not set by hand"
        )));
    }
    // The `github` wiring is consumed by the status-card mirror; a typo'd
    // value would silently mirror nothing, so it fails here where the setter
    // can see it.
    if tag_key == tags::GITHUB_KEY && crate::github::parse_wiring(value).is_none() {
        return Err(AppError::bad_request(format!(
            "invalid value '{value}' for '{tag_key}' — expected owner/name#number"
        )));
    }
    if !tags::is_valid_value(&tag_key, value) {
        return Err(AppError::bad_request(if tags::is_loud(&tag_key) {
            format!(
                "invalid value '{value}' for '{tag_key}' — expected one of {} (clear the tag to return to calm)",
                tags::ATTENTION_VALUES.join(", ")
            )
        } else {
            format!("invalid value '{value}' for '{tag_key}' — must be non-empty")
        }));
    }
    let by = author_or_manual(req.by.as_deref());
    let note = req.note.trim();
    tags::set(&st.db, &branch.id, &tag_key, value, note, &by).await?;
    tracing::info!(branch = %branch.id, tag = %tag_key, value = %value, "branch tag set");
    events::record_tag(&st.db, &st.bus, &branch.id, &tag_key, value, note, &by)
        .await
        .ok();
    Ok(Json(branch_view(&st.db, &branch).await?))
}

/// Clear a tag on a branch — the branch-scoped counterpart of
/// `sessions.tags.delete`.
pub(super) async fn clear_branch_tag(
    State(st): State<AppState>,
    Path((key, tag_key)): Path<(String, String)>,
    Query(q): Query<ByQuery>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    let by = author_or_manual(q.by.as_deref());
    tags::clear(&st.db, &branch.id, &tag_key).await?;
    tracing::info!(branch = %branch.id, tag = %tag_key, "branch tag cleared");
    events::record_tag(&st.db, &st.bus, &branch.id, &tag_key, "", "", &by)
        .await
        .ok();
    Ok(Json(branch_view(&st.db, &branch).await?))
}

// ---------------------------------------------------------------------------
// Operation registry — `branches.*`, bound onto
// `weaver_api::operations::branches`. Each handler below is the twin of a
// legacy axum handler above: same domain calls, same event/mirror
// side-effects, resolved from an operation's typed `Input` instead of a
// path/query/body triple. Authorization (actor policy, grants, and
// `require_branch_access` for the `Branch`-scoped ones) now happens once,
// centrally, in `web/operations.rs` — none of it is re-checked here. The
// legacy routes above stay live and untouched until the coordinated route
// deletion pass.
//
// `branches.events.list` reimplements [`branch_events`]'s one line
// (`events::history`, capped at 200 rows) rather than calling it, because
// that handler lives in `web/sessions.rs`, owned by another agent while this
// port is in flight; `events::history` is itself the shared logic, already
// used the same way by [`create_branch_event`] below and by
// `sessions.events.list`. `branches.issues.list` similarly calls
// `super::issues::issue_views` — the `pub(super)` mapping helper
// `list_branch_issues` (in `web/issues.rs`) already shares — rather than
// duplicating its projection.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<ops::list::List, _, _>(list_operation),
        register::<ops::get::Get, _, _>(get_operation),
        register::<ops::update::Update, _, _>(update_operation),
        register::<ops::status::set::Set, _, _>(status_set_operation),
        register::<ops::slack::reply::Reply, _, _>(slack_reply_operation),
        register::<ops::events::list::List, _, _>(events_list_operation),
        register::<ops::events::create::Create, _, _>(events_create_operation),
        register::<ops::tags::set::Set, _, _>(tags_set_operation),
        register::<ops::tags::delete::Delete, _, _>(tags_delete_operation),
        register::<ops::issues::list::List, _, _>(issues_list_operation),
        register::<slack_operations::connection_status::ConnectionStatus, _, _>(
            slack_connection_status_operation,
        ),
    ]
}

/// `branches.list` — the twin of [`list_branches`].
async fn list_operation(
    context: OperationContext,
    _input: ops::list::Input,
) -> ApiResult<ops::list::Output> {
    let st = context.state;
    let branches = branch_mod::list(&st.db).await?;
    let mut out: Vec<BranchView> = Vec::with_capacity(branches.len());
    for b in branches {
        out.push(branch_view(&st.db, &b).await?);
    }
    Ok(out)
}

/// `branches.get` — the twin of [`get_branch`].
async fn get_operation(
    context: OperationContext,
    input: ops::get::Input,
) -> ApiResult<ops::get::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    branch_view(&st.db, &branch).await
}

/// `branches.update` — the twin of [`patch_branch`].
async fn update_operation(
    context: OperationContext,
    input: ops::update::Input,
) -> ApiResult<ops::update::Output> {
    let st = context.state;
    let initial_branch = require_branch(&st.db, &input.branch).await?;
    let goal_sessions = if input.goal.is_some() {
        session_mod::handoff_capable_for_branch(&st.db, &initial_branch.id).await?
    } else {
        Vec::new()
    };
    let initial_goal_ids: std::collections::BTreeSet<_> = goal_sessions
        .iter()
        .map(|session| session.id.clone())
        .collect();
    let _goal_permits = st
        .launch_gate
        .acquire_sessions(goal_sessions.iter().map(|session| session.id.as_str()))
        .await;
    let branch = require_branch(&st.db, &initial_branch.id).await?;
    let current_goal_sessions = if input.goal.is_some() {
        session_mod::handoff_capable_for_branch(&st.db, &branch.id).await?
    } else {
        Vec::new()
    };
    if current_goal_sessions
        .iter()
        .any(|session| !initial_goal_ids.contains(&session.id))
    {
        return Err(AppError::conflict(
            "the branch's handoff-capable session changed while the goal edit was waiting; retry",
        ));
    }
    if let Some(title) = &input.title {
        let title = branch_mod::sanitize_user_title(title)
            .ok_or_else(|| AppError::bad_request("title must not be empty"))?;
        let expected_title = input.expected_title.as_deref().ok_or_else(|| {
            AppError::bad_request("expected_title is required when renaming a branch")
        })?;
        let expected_provenance = input
            .expected_title_provenance
            .as_deref()
            .ok_or_else(|| {
                AppError::bad_request(
                    "expected_title_provenance is required when renaming a branch",
                )
            })?
            .parse::<TitleProvenance>()
            .map_err(AppError::bad_request)?;
        match branch_mod::replace_title(
            &st.db,
            &branch.id,
            expected_title,
            expected_provenance,
            &title,
            TitleProvenance::User,
        )
        .await?
        {
            TitleUpdate::Applied(_) => {
                crate::channels::update_branch_channel_names(&st.db, &branch.id, &title).await?;
            }
            TitleUpdate::Stale(current) => {
                return Err(AppError::conflict(
                    "the task label changed while it was being edited; review it and retry",
                )
                .with_fields(json!({ "branch": branch_view(&st.db, &current).await? })));
            }
            TitleUpdate::Missing => return Err(AppError::not_found("branch")),
        }
    }
    if let Some(goal) = &input.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal, "user").await?;
        for session in &current_goal_sessions {
            session_mod::bump_mutation_revision(&st.db, &session.id).await?;
            crate::channels::update_session_goal(&st.db, &session.id, goal).await?;
        }
    }
    if let Some(description) = &input.description {
        branch_mod::set_description(&st.db, &branch.id, description).await?;
    }
    if input.title.is_some() || input.goal.is_some() || input.description.is_some() {
        tracing::info!(
            branch = %branch.id,
            title = input.title.is_some(),
            goal = input.goal.is_some(),
            description = input.description.is_some(),
            "branch patched"
        );
    }
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    branch_view(&st.db, &branch).await
}

/// `branches.status.set` — the twin of [`set_branch_status`].
async fn status_set_operation(
    context: OperationContext,
    input: ops::status::set::Input,
) -> ApiResult<ops::status::set::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let level = input.level.trim().to_ascii_lowercase();
    if level != CALM_STATUS && !tags::is_valid_value(tags::ATTENTION_KEY, &level) {
        return Err(AppError::bad_request(format!(
            "unknown status '{level}' — expected one of {CALM_STATUS}, {}",
            tags::ATTENTION_VALUES.join(", ")
        )));
    }
    let message = input
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    if let Some(message) = message {
        branch_mod::set_description(&st.db, &branch.id, message).await?;
    }
    let value = if level == CALM_STATUS {
        tags::clear(&st.db, &branch.id, tags::ATTENTION_KEY).await?;
        String::new()
    } else {
        tags::set(&st.db, &branch.id, tags::ATTENTION_KEY, &level, "", "agent").await?;
        level.clone()
    };
    tracing::info!(branch = %branch.id, level = %level, "branch status set");
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "tag",
        json!({
            "key": tags::ATTENTION_KEY,
            "value": value,
            "note": message.unwrap_or_default(),
            "by": "agent",
        }),
    )
    .await?;
    if let Some(channel_id) =
        crate::channels::session_channel_for_branch(&st.db, &branch.id).await?
    {
        let urgency = crate::channels::Urgency::from_status_level(&level);
        let author =
            crate::channels::Subject::new(crate::channels::SubjectKind::Session, &channel_id);
        crate::channels::append(
            &st.db,
            &channel_id,
            crate::channels::NewMessage {
                kind: crate::channels::MessageKind::Status,
                urgency,
                author: &author,
                body: message.unwrap_or(&level),
                payload: &json!({ "level": level }),
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await?;
    }
    crate::slack::spawn_status_mirrors(st.clone(), branch.id.clone());
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    branch_view(&st.db, &branch).await
}

/// `branches.slack.reply` — the twin of [`slack_reply`]. Backs the
/// `loom_messaging::slack_reply` MCP tool, so an agent posts through this same
/// operation rather than a separate path.
async fn slack_reply_operation(
    context: OperationContext,
    input: ops::slack::reply::Input,
) -> ApiResult<ops::slack::reply::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let text = input.text.trim();
    if text.is_empty() {
        return Err(AppError::bad_request("text is required"));
    }
    if input.thread.is_none() {
        // Preserve the compatibility route while making the session channel the
        // canonical reply record and delivery bus. The branch wiring is checked
        // before appending, matching this endpoint's historical failure shape.
        let wired = tags::get(&st.db, &branch.id, crate::slack::WIRED_TAG)
            .await?
            .ok_or_else(|| AppError::bad_request("this branch is not wired to a Slack thread"))?;
        if crate::slack::parse_wiring(&wired.value).is_none() {
            return Err(AppError::bad_request("malformed slack wiring tag"));
        }
        let channel_id = crate::channels::session_channel_for_branch(&st.db, &branch.id)
            .await?
            .ok_or_else(|| AppError::bad_request("this branch has no open session channel"))?;
        let channel = crate::channels::access(&st.db, &channel_id)
            .await?
            .ok_or_else(|| AppError::not_found("channel"))?;
        let author =
            crate::channels::Subject::new(crate::channels::SubjectKind::Session, &channel_id);
        let request = CreateChannelMessageReq {
            kind: "result".to_string(),
            urgency: "normal".to_string(),
            body: text.to_string(),
            payload: json!({ "compatibility_source": "slack_reply" }),
            reply_to: None,
            idempotency_key: input.idempotency_key,
        };
        let (inserted, message) =
            super::channels::append_and_deliver(&st, &channel_id, &channel, &author, &request)
                .await?;
        super::channels::record_channel_message_event(
            &st,
            &channel_id,
            &author,
            &message,
            inserted,
        )
        .await;
        let delivery = message
            .deliveries
            .iter()
            .find(|delivery| delivery.binding_id == weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID)
            .ok_or_else(|| AppError::bad_request("this branch is not wired to a Slack thread"))?;
        if delivery.state == "failed" {
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                delivery
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "Slack delivery failed".to_string()),
            ));
        }
        return Ok(json!({
            "posted": delivery.state == "delivered",
            "ts": delivery.external_id,
            "message_id": message.id,
            "delivery": delivery,
        }));
    }

    let target = input
        .thread
        .as_ref()
        .ok_or_else(|| AppError::bad_request("thread is required"))?;
    let (channel, root) = crate::slack::parse_thread_ref(target).map_err(AppError::bad_request)?;
    if !crate::slack_routes::allows(&st.db, &branch.id, &channel, &root).await? {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "this session holds no Slack route for that thread",
        ));
    }
    let web = crate::slack::SlackWeb::from_db(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("Slack is not configured on this server"))?;
    let ts = web
        .post_message(&channel, Some(&root), text)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(json!({ "posted": true, "ts": ts }))
}

/// `branches.events.list` — a bounded `Vec<Event>` read (the last 200 rows),
/// not a live stream; see the module doc above. Same query
/// [`branch_events`](super::sessions::branch_events) runs (that handler lives
/// in `web/sessions.rs`, owned elsewhere for the duration of this port).
async fn events_list_operation(
    context: OperationContext,
    input: ops::events::list::Input,
) -> ApiResult<ops::events::list::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    Ok(events::history(&st.db, &branch.id, 200).await?)
}

/// `branches.events.create` — the twin of [`create_branch_event`].
async fn events_create_operation(
    context: OperationContext,
    input: ops::events::create::Input,
) -> ApiResult<ops::events::create::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let kind = input.kind.trim();
    if kind.is_empty() {
        return Err(AppError::bad_request("event kind is required"));
    }
    let event = events::record(&st.db, &st.bus, &branch.id, kind, input.data).await?;
    tracing::info!(branch = %branch.id, kind = %kind, "branch event created");
    Ok(event)
}

/// `branches.tags.set` — the twin of [`set_branch_tag`].
async fn tags_set_operation(
    context: OperationContext,
    input: ops::tags::set::Input,
) -> ApiResult<ops::tags::set::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let value = input.value.trim();
    if crate::github::is_reserved_tag(&input.key) {
        return Err(AppError::bad_request(format!(
            "'{}' is loom-internal bookkeeping — it can be cleared, not set by hand",
            input.key
        )));
    }
    // The `github` wiring is consumed by the status-card mirror; a typo'd
    // value would silently mirror nothing, so it fails here where the setter
    // can see it.
    if input.key == tags::GITHUB_KEY && crate::github::parse_wiring(value).is_none() {
        return Err(AppError::bad_request(format!(
            "invalid value '{value}' for '{}' — expected owner/name#number",
            input.key
        )));
    }
    if !tags::is_valid_value(&input.key, value) {
        return Err(AppError::bad_request(if tags::is_loud(&input.key) {
            format!(
                "invalid value '{value}' for '{}' — expected one of {} (clear the tag to return to calm)",
                input.key,
                tags::ATTENTION_VALUES.join(", ")
            )
        } else {
            format!(
                "invalid value '{value}' for '{}' — must be non-empty",
                input.key
            )
        }));
    }
    let by = author_or_manual(input.by.as_deref());
    let note = input.note.trim();
    tags::set(&st.db, &branch.id, &input.key, value, note, &by).await?;
    tracing::info!(branch = %branch.id, tag = %input.key, value = %value, "branch tag set");
    events::record_tag(&st.db, &st.bus, &branch.id, &input.key, value, note, &by)
        .await
        .ok();
    branch_view(&st.db, &branch).await
}

/// `branches.tags.delete` — the twin of [`clear_branch_tag`].
async fn tags_delete_operation(
    context: OperationContext,
    input: ops::tags::delete::Input,
) -> ApiResult<ops::tags::delete::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let by = author_or_manual(input.by.as_deref());
    tags::clear(&st.db, &branch.id, &input.key).await?;
    tracing::info!(branch = %branch.id, tag = %input.key, "branch tag cleared");
    events::record_tag(&st.db, &st.bus, &branch.id, &input.key, "", "", &by)
        .await
        .ok();
    branch_view(&st.db, &branch).await
}

/// `branches.issues.list` — the twin of
/// [`list_branch_issues`](super::issues::list_branch_issues) (`web/issues.rs`,
/// not this file). Reuses that module's `pub(super) fn issue_views` mapping
/// helper rather than duplicating the issue → `IssueView` projection.
async fn issues_list_operation(
    context: OperationContext,
    input: ops::issues::list::Input,
) -> ApiResult<ops::issues::list::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let issues =
        weaver_core::issue::list_for_branch(&st.db, &branch.repo_root, &branch.branch, input.all)
            .await?;
    super::issues::issue_views(&st.db, issues).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn test_state(db: Db) -> AppState {
        AppState {
            ctx: crate::Ctx {
                db: db.clone(),
                bus: crate::events::EventBus::new(),
                addr: "127.0.0.1:0".to_string(),
            },
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        }
    }

    #[tokio::test]
    async fn set_branch_status_sets_then_clears_attention_with_one_event_each() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();

        let view = set_branch_status(
            State(st.clone()),
            Path(branch.id.clone()),
            Json(BranchStatusReq {
                level: "attention".to_string(),
                message: Some("need review".to_string()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(view.description, "need review");
        let attention = view
            .tags
            .iter()
            .find(|t| t.key == tags::ATTENTION_KEY)
            .expect("attention tag set");
        assert_eq!(attention.value, "attention");

        let view = set_branch_status(
            State(st.clone()),
            Path(branch.id.clone()),
            Json(BranchStatusReq {
                level: "ok".to_string(),
                message: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(
            view.tags.iter().all(|t| t.key != tags::ATTENTION_KEY),
            "ok clears the tag rather than storing it"
        );
        // The message is untouched by a bare `ok` — the tag event is what
        // moved, not the description.
        assert_eq!(view.description, "need review");

        let events = events::history(&db, &branch.id, 10).await.unwrap();
        assert_eq!(events.len(), 2, "one tag event per status call");
        assert!(events.iter().all(|e| e.kind == "tag"));
    }

    #[tokio::test]
    async fn set_branch_status_rejects_an_unknown_level() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();
        let err = set_branch_status(
            State(st),
            Path(branch.id),
            Json(BranchStatusReq {
                level: "urgent".to_string(),
                message: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_branch_event_persists_and_publishes_to_the_bus() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();
        let mut rx = st.bus.subscribe();

        let event = create_branch_event(
            State(st),
            Path(branch.id.clone()),
            Json(CreateEventReq {
                kind: "hook".to_string(),
                data: json!({ "event": "working" }),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(event.kind, "hook");

        let published = rx.try_recv().expect("published to the bus");
        assert_eq!(published.branch_id, branch.id);
        assert_eq!(published.kind, "hook");

        let history = events::history(&db, &branch.id, 10).await.unwrap();
        assert_eq!(history.len(), 1);
    }

    #[tokio::test]
    async fn branch_tags_set_and_clear_without_a_live_session() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();
        // No session row exists for this branch at all — this is exactly the
        // case `require_session` rejects and `require_branch` accepts.

        let view = set_branch_tag(
            State(st.clone()),
            Path((branch.id.clone(), "triage".to_string())),
            Json(TagReq {
                value: "blocked".to_string(),
                note: "flaky test".to_string(),
                by: Some("watch-x".to_string()),
            }),
        )
        .await
        .unwrap()
        .0;
        let tag = view.tags.iter().find(|t| t.key == "triage").unwrap();
        assert_eq!(tag.value, "blocked");
        assert_eq!(tag.set_by, "watch-x");

        let view = clear_branch_tag(
            State(st),
            Path((branch.id.clone(), "triage".to_string())),
            Query(ByQuery { by: None }),
        )
        .await
        .unwrap()
        .0;
        assert!(view.tags.iter().all(|t| t.key != "triage"));
    }
}
