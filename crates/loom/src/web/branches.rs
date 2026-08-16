use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use weaver_api::{BranchStatusReq, BranchView, CreateChannelMessageReq, CreateEventReq, TagReq};
use weaver_core::branch::{TitleProvenance, TitleUpdate};
use weaver_core::{branch as branch_mod, config, tags};

use crate::{events, session as session_mod};

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
/// both the input that clears the tag and the value `weaver status` reads
/// back as the default.
const CALM_STATUS: &str = "ok";

/// Set the agent's attention level and current-state message in one call:
/// validate the level, write the description when a message is given,
/// set-or-clear the `attention` tag, and record exactly one `tag` event —
/// what `weaver status set --tag <level> [--message <message>]` does against the local
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

/// `GET /api/slack/status` — the state of every link in the Slack trigger path,
/// for the Connections settings pane.
///
/// One `connected` boolean is not enough to run this integration: a deployment
/// can hold a live socket and still discard every mention — because the bot
/// token is a person's rather than the app's, because the app-level token opened
/// a different app than the bot belongs to, because no repository is set, or
/// because the access list excludes everyone. Each link is reported separately
/// so the pane can name the one that is broken instead of reporting health.
pub(super) async fn slack_status(State(st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let app_token_set = !crate::slack::app_token(&st.db).await.is_empty();
    let bot_token_set = !crate::slack::bot_token(&st.db).await.is_empty();
    let enabled = crate::slack::is_enabled(&st.db).await;

    // `auth.test` proves the bot credential still works and says who loom is —
    // including whether the token belongs to the app or to a person.
    let identity = match crate::slack::SlackWeb::from_db(&st.db).await {
        Some(web) => match web.auth_test().await {
            Ok(id) => json!({
                "user_id": id.user_id,
                "team_id": id.team_id,
                "token_kind": if id.is_bot() { "bot" } else { "user" },
                "error": serde_json::Value::Null,
            }),
            Err(e) => json!({ "error": e.to_string() }),
        },
        None => serde_json::Value::Null,
    };

    let access = match crate::slack::access(&st.db).await {
        crate::slack::Access::Workspace => json!({ "mode": "workspace", "users": [] }),
        crate::slack::Access::Listed(users) => json!({ "mode": "listed", "users": users }),
    };

    Ok(Json(json!({
        "enabled": enabled,
        "app_token_set": app_token_set,
        "bot_token_set": bot_token_set,
        "configured": app_token_set && bot_token_set,
        "identity": identity,
        "access": access,
        "default_repo": config::get(&st.db, "slack.default_repo")
            .await
            .unwrap_or_default()
            .trim(),
        // What the supervisor is actually doing, as opposed to what a fresh
        // credential probe suggests it could do.
        "socket": crate::slack::health(),
    })))
}

/// Append a raw event row to a branch's log — the escape hatch for an event
/// kind with no dedicated mutating route of its own (namely `weaver hook`,
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

/// Set (upsert) a tag on a branch — the branch-scoped twin of
/// [`set_session_tag`], for a `weaver tag` target with no live session (a
/// finished session, or `--session` pointing at another branch entirely).
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

/// Clear a tag on a branch — the branch-scoped twin of [`clear_session_tag`].
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
