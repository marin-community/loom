use serde_json::json;
use weaver_api::operations::branches as ops;
use weaver_api::operations::slack as slack_operations;
use weaver_api::BranchView;
use weaver_core::branch::{TitleProvenance, TitleUpdate};
use weaver_core::{branch as branch_mod, config, tags};

use crate::{events, session as session_mod};

use super::operations::{register, Bound, OperationContext};
use super::{author_or_manual, branch_view, require_branch};
use super::{ApiResult, AppError, AppState};
use axum::http::StatusCode;

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

/// The attention value that means "calm" — never stored (absence is calm);
/// both the input that clears the tag and the value `loom status` reads
/// back as the default.
const CALM_STATUS: &str = "ok";

/// `state`'s serialized name, matching `SocketState`'s own `#[serde(rename_all =
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
/// settings pane, backing `slack.connection_status`.
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

    // What the supervisor is actually doing right now.
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

async fn slack_connection_status_operation(
    context: OperationContext,
    _input: slack_operations::connection_status::Input,
) -> ApiResult<slack_operations::connection_status::Output> {
    Ok(slack_connection_status_view(&context.state).await)
}

// ---------------------------------------------------------------------------
// Operation registry — `branches.*`, bound onto
// `weaver_api::operations::branches`. Authorization (actor policy, grants,
// and `require_branch_access` for the `Branch`-scoped ones) happens once,
// centrally, in `web/operations.rs` — none of it is re-checked here.
//
// `branches.events.list` and `sessions.events.list` (`web/sessions.rs`) both
// read through `events::history`, capped at 200 rows. `branches.issues.list`
// shares `super::issues::issue_views` with the other call sites in
// `web/issues.rs`.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<ops::list::Op, _, _>(list_operation),
        register::<ops::get::Op, _, _>(get_operation),
        register::<ops::update::Op, _, _>(update_operation),
        register::<ops::status::set::Op, _, _>(status_set_operation),
        register::<ops::slack::send::Op, _, _>(slack_send_operation),
        register::<ops::events::list::Op, _, _>(events_list_operation),
        register::<ops::events::create::Op, _, _>(events_create_operation),
        register::<ops::tags::set::Op, _, _>(tags_set_operation),
        register::<ops::tags::delete::Op, _, _>(tags_delete_operation),
        register::<ops::issues::list::Op, _, _>(issues_list_operation),
        register::<slack_operations::connection_status::Op, _, _>(
            slack_connection_status_operation,
        ),
    ]
}

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

async fn get_operation(
    context: OperationContext,
    input: ops::get::Input,
) -> ApiResult<ops::get::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    branch_view(&st.db, &branch).await
}

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

async fn slack_send_operation(
    context: OperationContext,
    input: ops::slack::send::Input,
) -> ApiResult<ops::slack::send::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let text = input.text.trim();
    if text.is_empty() {
        return Err(AppError::bad_request("text is required"));
    }
    let (channel, root) =
        crate::slack::parse_thread_ref(&input.thread).map_err(AppError::bad_request)?;
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
        .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(json!({ "posted": true, "ts": ts }))
}

/// `branches.events.list` — a bounded `Vec<Event>` read (the last 200 rows),
/// not a live stream; see the module doc above. Runs the same query as
/// `sessions.events.list` (`web/sessions.rs`).
async fn events_list_operation(
    context: OperationContext,
    input: ops::events::list::Input,
) -> ApiResult<ops::events::list::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    Ok(events::history(&st.db, &branch.id, 200).await?)
}

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

/// `branches.issues.list`. Reuses [`super::issues::issue_views`] rather than
/// duplicating the issue → `IssueView` mapping.
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
    use crate::auth::{Grant, Principal};
    use crate::db::Db;

    /// The operation handlers take their caller as an `OperationContext`; who
    /// may reach them at all is `actor` on the declaration, checked before any
    /// of this runs, so these tests exercise the behaviour and not the gate.
    fn admin_context(state: AppState) -> OperationContext {
        OperationContext::new(
            state,
            Principal {
                username: "test".to_string(),
                github_login: None,
                via: crate::auth::AuthVia::Loopback,
                grant: Grant::Admin,
                automation_context: None,
            },
        )
    }

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

        let view = status_set_operation(
            admin_context(st.clone()),
            ops::status::set::Input {
                level: "attention".to_string(),
                message: Some("need review".to_string()),
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(view.description, "need review");
        let attention = view
            .tags
            .iter()
            .find(|t| t.key == tags::ATTENTION_KEY)
            .expect("attention tag set");
        assert_eq!(attention.value, "attention");

        let view = status_set_operation(
            admin_context(st.clone()),
            ops::status::set::Input {
                level: "ok".to_string(),
                message: None,
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
        assert!(
            view.tags.iter().all(|t| t.key != tags::ATTENTION_KEY),
            "ok clears the tag rather than storing it"
        );
        // A bare `ok` clears the tag; the description is untouched.
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
        let err = status_set_operation(
            admin_context(st),
            ops::status::set::Input {
                level: "urgent".to_string(),
                message: None,
                branch: branch.id,
            },
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

        let event = events_create_operation(
            admin_context(st),
            ops::events::create::Input {
                kind: "hook".to_string(),
                data: json!({ "event": "working" }),
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
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
        // No session row exists for this branch — the case `require_session`
        // rejects and `require_branch` accepts.

        let view = tags_set_operation(
            admin_context(st.clone()),
            ops::tags::set::Input {
                key: "triage".to_string(),
                value: "blocked".to_string(),
                note: "flaky test".to_string(),
                by: Some("watch-x".to_string()),
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
        let tag = view.tags.iter().find(|t| t.key == "triage").unwrap();
        assert_eq!(tag.value, "blocked");
        assert_eq!(tag.set_by, "watch-x");

        let view = tags_delete_operation(
            admin_context(st),
            ops::tags::delete::Input {
                key: "triage".to_string(),
                by: None,
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
        assert!(view.tags.iter().all(|t| t.key != "triage"));
    }
}
