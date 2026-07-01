use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use weaver_api::{BranchGoalReq, BranchStatusReq, BranchView, CreateEventReq, TagReq};
use weaver_core::branch as branch_mod;
use weaver_core::tags;

use crate::events;

use super::sessions::ByQuery;
use super::{author_or_manual, branch_view, require_branch};
use super::{ApiResult, AppError, AppState};

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
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    Ok(Json(branch_view(&st.db, &branch).await?))
}

/// Set the goal atomically: derive a title when one isn't set yet, and record
/// the `goal_set` event — the one-call semantics `weaver goal set` has always
/// had against the local database, now reproduced server-side rather than
/// composed client-side out of a generic `PATCH`.
pub(super) async fn set_branch_goal(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<BranchGoalReq>,
) -> ApiResult<Json<BranchView>> {
    let branch = require_branch(&st.db, &key).await?;
    let goal = req.goal.trim();
    if goal.is_empty() {
        return Err(AppError::bad_request(
            "a goal is required — pass text, --file <path>, or pipe via '-'",
        ));
    }
    branch_mod::set_goal(&st.db, &branch.id, goal, "agent").await?;
    if branch.title.is_empty() {
        let title = branch_mod::derive_title(goal);
        branch_mod::set_title(&st.db, &branch.id, &title).await?;
    }
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "goal_set",
        json!({ "goal": goal }),
    )
    .await
    .ok();
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
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "tag",
        json!({ "key": tags::ATTENTION_KEY, "value": value, "note": "", "by": "agent" }),
    )
    .await
    .ok();
    let branch = branch_mod::get(&st.db, &branch.id)
        .await?
        .ok_or_else(|| AppError::not_found("branch"))?;
    Ok(Json(branch_view(&st.db, &branch).await?))
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
        }
    }

    #[tokio::test]
    async fn set_branch_goal_derives_title_and_records_one_event() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();
        assert!(branch.title.is_empty());

        let view = set_branch_goal(
            State(st.clone()),
            Path(branch.id.clone()),
            Json(BranchGoalReq {
                goal: "Ship the thing\nmore detail".to_string(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(view.goal, "Ship the thing\nmore detail");
        assert_eq!(view.title, "Ship the thing", "an empty title is derived");

        let events = events::history(&db, &branch.id, 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "goal_set");

        // A pre-existing title is left alone.
        let view2 = set_branch_goal(
            State(st),
            Path(branch.id.clone()),
            Json(BranchGoalReq {
                goal: "a new goal".to_string(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(view2.title, "Ship the thing");
    }

    #[tokio::test]
    async fn set_branch_goal_rejects_empty() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();
        let err = set_branch_goal(
            State(st),
            Path(branch.id),
            Json(BranchGoalReq {
                goal: "   ".to_string(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
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
                by: Some("overlooker-x".to_string()),
            }),
        )
        .await
        .unwrap()
        .0;
        let tag = view.tags.iter().find(|t| t.key == "triage").unwrap();
        assert_eq!(tag.value, "blocked");
        assert_eq!(tag.set_by, "overlooker-x");

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
