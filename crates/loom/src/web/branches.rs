use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use weaver_api::{BranchStatusReq, BranchView, CreateEventReq, TagReq};
use weaver_core::branch as branch_mod;
use weaver_core::tags;

use crate::events;

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
    goal: Option<String>,
    description: Option<String>,
}

pub(super) async fn patch_branch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchBranchReq>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    if let Some(title) = &req.title {
        branch_mod::set_title(&st.db, &branch.id, title).await?;
    }
    if let Some(goal) = &req.goal {
        branch_mod::set_goal(&st.db, &branch.id, goal, "user").await?;
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
/// what `weaver status <level> [message]` has always done against the local
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
/// to its wired Slack thread. The token stays on the server: the agent (holding
/// `LOOM_TOKEN`) calls this route rather than being handed the workspace-wide
/// bot token, and loom derives the destination from the branch's `slack` wiring
/// tag. The Slack analog of the GitHub-triggered session replying with `gh`.
#[derive(Deserialize)]
pub(super) struct SlackReplyReq {
    pub text: String,
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
    let wired = tags::get(&st.db, &branch.id, crate::slack::WIRED_TAG)
        .await?
        .ok_or_else(|| AppError::bad_request("this branch is not wired to a Slack thread"))?;
    let (_team, channel, root) = crate::slack::parse_wiring(&wired.value)
        .ok_or_else(|| AppError::bad_request("malformed slack wiring tag"))?;
    let web = crate::slack::SlackWeb::from_db(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("Slack is not configured on this server"))?;
    let ts = web
        .post_message(&channel, Some(&root), text)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(json!({ "posted": true, "ts": ts })))
}

/// `GET /api/slack/status` — read-only connection state for the Connections
/// settings pane. `connected` reflects whether an `auth.test` with the bot token
/// succeeds (a credential-health proxy for the live socket).
pub(super) async fn slack_status(State(st): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let configured = !crate::slack::app_token(&st.db).await.is_empty()
        && !crate::slack::bot_token(&st.db).await.is_empty();
    let enabled = crate::slack::is_enabled(&st.db).await;
    let (connected, bot_user, team, error) = if configured {
        match crate::slack::SlackWeb::from_db(&st.db).await {
            Some(web) => match web.auth_test().await {
                Ok(id) => (true, Some(id.user_id), Some(id.team_id), None),
                Err(e) => (false, None, None, Some(e.to_string())),
            },
            None => (false, None, None, None),
        }
    } else {
        (false, None, None, None)
    };
    Ok(Json(json!({
        "configured": configured,
        "enabled": enabled,
        "connected": connected,
        "bot_user": bot_user,
        "team": team,
        "error": error,
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
            db: db.clone(),
            bus: crate::events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
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
