use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{
    ChannelBindingView, ChannelMessageView, ChannelSubscriptionView, ChannelView,
    CreateChannelMessageReq, CreateChannelReq, SendReq, SetChannelReadMarkerReq,
    SetChannelSubscriptionReq,
};

use crate::{
    auth::{Grant, Principal},
    channels::{self, MessageKind, NewMessage, Subject, SubjectKind, SubscriptionMode, Urgency},
    events,
};

use super::{ApiResult, AppError, AppState};

const MAX_NAME_LEN: usize = 120;
const MAX_TOPIC_LEN: usize = 4_096;
const MAX_BODY_LEN: usize = 256 * 1024;

#[derive(Debug, Default, Deserialize)]
pub(super) struct ListChannelsQuery {
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ListMessagesQuery {
    #[serde(default)]
    after: i64,
    limit: Option<usize>,
}

pub(super) fn principal_subject(principal: &Principal) -> Subject {
    match &principal.grant {
        Grant::Session { session_id, .. } => Subject::new(SubjectKind::Session, session_id),
        Grant::Automation { subject, .. } => Subject::new(SubjectKind::Automation, subject),
        Grant::Admin | Grant::User => Subject::new(SubjectKind::User, &principal.username),
    }
}

pub(super) async fn list_channels(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<ListChannelsQuery>,
) -> ApiResult<Json<Vec<ChannelView>>> {
    let subject = principal_subject(&principal);
    let channels = match &principal.grant {
        Grant::Session { session_id, .. } => {
            channels::list_for_session_tree(&st.db, session_id, &subject, query.archived).await?
        }
        _ => channels::list_all(&st.db, &subject, query.archived).await?,
    };
    Ok(Json(channels))
}

pub(super) async fn create_channel(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreateChannelReq>,
) -> ApiResult<(StatusCode, Json<ChannelView>)> {
    let name = req.name.trim();
    let topic = req.topic.trim();
    validate_text("name", name, 1, MAX_NAME_LEN)?;
    validate_text("topic", topic, 0, MAX_TOPIC_LEN)?;
    let (repo_root, branch_id) = match &principal.grant {
        Grant::Session { branch_id, .. } => {
            let repo_root: String =
                sqlx::query_scalar("SELECT repo_root FROM branches WHERE id = ?")
                    .bind(branch_id)
                    .fetch_optional(&st.db)
                    .await?
                    .ok_or_else(|| AppError::not_found("branch"))?;
            (repo_root, Some(branch_id.as_str()))
        }
        _ => {
            let repo_root = req
                .repo_root
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::bad_request("repo_root is required"))?;
            (repo_root.to_string(), None)
        }
    };
    let subject = principal_subject(&principal);
    let channel =
        channels::create_custom(&st.db, &repo_root, branch_id, name, topic, &subject).await?;
    events::record_system(
        &st.db,
        &st.bus,
        "channel_created",
        json!({ "channel_id": channel.id, "by": subject.id }),
    )
    .await
    .ok();
    Ok((StatusCode::CREATED, Json(channel)))
}

pub(super) async fn get_channel(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<ChannelView>> {
    let subject = principal_subject(&principal);
    let channel = channels::get(&st.db, &id, &subject)
        .await?
        .ok_or_else(|| AppError::not_found("channel"))?;
    Ok(Json(channel))
}

pub(super) async fn list_channel_messages(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
) -> ApiResult<Json<Vec<ChannelMessageView>>> {
    require_channel(&st, &id).await?;
    if query
        .limit
        .is_some_and(|limit| !(1..=weaver_api::CHANNEL_MESSAGE_LIMIT_MAX).contains(&limit))
    {
        return Err(AppError::bad_request(format!(
            "limit must be between 1 and {}",
            weaver_api::CHANNEL_MESSAGE_LIMIT_MAX
        )));
    }
    let mut messages = channels::messages(&st.db, &id, query.after.max(0)).await?;
    if let Some(limit) = query.limit {
        messages.truncate(limit);
    }
    Ok(Json(messages))
}

pub(super) async fn list_channel_bindings(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<ChannelBindingView>>> {
    let channel = require_channel(&st, &id).await?;
    Ok(Json(channel_bindings(&st, &id, &channel).await?))
}

pub(super) async fn create_channel_message(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<CreateChannelMessageReq>,
) -> ApiResult<(StatusCode, Json<ChannelMessageView>)> {
    let channel = require_channel(&st, &id).await?;
    let author = principal_subject(&principal);
    let (inserted, message) = append_and_deliver(&st, &id, &channel, &author, &req).await?;

    record_channel_message_event(&st, &id, &author, &message, inserted).await;
    Ok((
        if inserted {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(message),
    ))
}

pub(super) async fn record_channel_message_event(
    st: &AppState,
    channel_id: &str,
    author: &Subject,
    message: &ChannelMessageView,
    inserted: bool,
) {
    if !inserted {
        return;
    }
    events::record_system(
        &st.db,
        &st.bus,
        "channel_message",
        json!({
            "channel_id": channel_id,
            "message_id": message.id,
            "kind": message.kind,
            "urgency": message.urgency,
            "by": author.id,
        }),
    )
    .await
    .ok();
}

pub(super) async fn append_and_deliver(
    st: &AppState,
    id: &str,
    channel: &channels::ChannelAccess,
    author: &Subject,
    req: &CreateChannelMessageReq,
) -> ApiResult<(bool, ChannelMessageView)> {
    if channel.state != channels::OPEN_STATE {
        return Err(AppError::conflict("channel is archived"));
    }
    let kind = MessageKind::parse(&req.kind)
        .ok_or_else(|| AppError::bad_request("unknown channel message kind"))?;
    let urgency = Urgency::parse(&req.urgency)
        .ok_or_else(|| AppError::bad_request("unknown channel message urgency"))?;
    let body = req.body.trim();
    validate_text("body", body, 1, MAX_BODY_LEN)?;
    let idempotency_key = req.idempotency_key.as_deref().map(str::trim);
    if let Some(key) = idempotency_key {
        validate_text(
            "idempotency_key",
            key,
            1,
            weaver_api::CHANNEL_IDEMPOTENCY_KEY_MAX_LEN,
        )?;
    }
    if let Some(reply_to) = req.reply_to.as_deref() {
        let belongs: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM channel_messages WHERE id = ? AND channel_id = ?
             )",
        )
        .bind(reply_to)
        .bind(id)
        .fetch_one(&st.db)
        .await?;
        if !belongs {
            return Err(AppError::bad_request(
                "reply_to must identify a message in this channel",
            ));
        }
    }
    let outcome = channels::append_with_outcome(
        &st.db,
        id,
        NewMessage {
            kind,
            urgency,
            author,
            body,
            payload: &req.payload,
            reply_to: req.reply_to.as_deref(),
            idempotency_key,
        },
    )
    .await?;
    let inserted = outcome.inserted;
    let message_id = outcome.message.id;

    // Session channels are durable inboxes; custom channels become inboxes for
    // agents that explicitly subscribe in `deliver` mode. Only ordinary
    // conversation reaches runtimes, and an agent never prompts itself.
    if kind == MessageKind::Message {
        for target in channels::delivery_targets(&st.db, id).await? {
            if author.kind == SubjectKind::Session && author.id == target {
                continue;
            }
            let binding_id = weaver_api::channel_session_binding_id(&target);
            channels::create_delivery(&st.db, &message_id, &binding_id, "session", Some(&target))
                .await?;
            if channels::delivery_succeeded(&st.db, &message_id, &binding_id).await? {
                continue;
            }
            let delivery_error = super::sessions::send_session(
                State(st.clone()),
                Path(target.clone()),
                Json(SendReq {
                    text: body.to_string(),
                    submit: true,
                    by: Some(format!("channel:{}", author.id)),
                }),
            )
            .await
            .err()
            .map(|error| error.message().to_string());
            channels::finish_delivery(
                &st.db,
                &message_id,
                &binding_id,
                delivery_error.as_deref(),
                None,
            )
            .await?;
        }
    }

    // A session-authored message or result on its own channel is the canonical
    // reply stream. A Slack-origin binding fans it out while the durable channel
    // item and idempotency key remain the source of truth.
    if matches!(kind, MessageKind::Message | MessageKind::Result)
        && author.kind == SubjectKind::Session
        && channel.session_id.as_deref() == Some(author.id.as_str())
    {
        deliver_to_origin_slack(st, channel, &message_id, body).await?;
    }

    Ok((
        inserted,
        channels::refresh_message(&st.db, &message_id).await?,
    ))
}

async fn channel_bindings(
    st: &AppState,
    id: &str,
    channel: &channels::ChannelAccess,
) -> ApiResult<Vec<ChannelBindingView>> {
    let mut bindings = channels::delivery_targets(&st.db, id)
        .await?
        .into_iter()
        .map(|target| ChannelBindingView {
            id: weaver_api::channel_session_binding_id(&target),
            kind: "session".to_string(),
            label: if channel.session_id.as_deref() == Some(target.as_str()) {
                "this session".to_string()
            } else {
                target.clone()
            },
            target_session_id: Some(target),
        })
        .collect::<Vec<_>>();
    if origin_slack_target(st, channel).await?.is_some() {
        bindings.push(ChannelBindingView {
            id: weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID.to_string(),
            kind: "slack_thread".to_string(),
            label: "origin Slack thread".to_string(),
            target_session_id: None,
        });
    }
    Ok(bindings)
}

async fn origin_slack_target(
    st: &AppState,
    channel: &channels::ChannelAccess,
) -> ApiResult<Option<(String, String)>> {
    let Some(branch_id) = channel.branch_id.as_deref() else {
        return Ok(None);
    };
    let Some(wired) = weaver_core::tags::get(&st.db, branch_id, crate::slack::WIRED_TAG).await?
    else {
        return Ok(None);
    };
    Ok(crate::slack::parse_wiring(&wired.value).map(|(_, channel, root)| (channel, root)))
}

async fn deliver_to_origin_slack(
    st: &AppState,
    channel: &channels::ChannelAccess,
    message_id: &str,
    body: &str,
) -> ApiResult<()> {
    let Some((slack_channel, root)) = origin_slack_target(st, channel).await? else {
        return Ok(());
    };
    const BINDING_ID: &str = weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID;
    channels::create_delivery(&st.db, message_id, BINDING_ID, "slack_thread", None).await?;
    if channels::delivery_succeeded(&st.db, message_id, BINDING_ID).await? {
        return Ok(());
    }
    let Some(web) = crate::slack::SlackWeb::from_db(&st.db).await else {
        channels::finish_delivery(
            &st.db,
            message_id,
            BINDING_ID,
            Some("Slack is not configured on this server"),
            None,
        )
        .await?;
        return Ok(());
    };
    match web.post_message(&slack_channel, Some(&root), body).await {
        Ok(ts) => {
            channels::finish_delivery(&st.db, message_id, BINDING_ID, None, Some(&ts)).await?
        }
        Err(error) => {
            channels::finish_delivery(
                &st.db,
                message_id,
                BINDING_ID,
                Some(&error.to_string()),
                None,
            )
            .await?
        }
    }
    Ok(())
}

pub(super) async fn set_channel_subscription(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<SetChannelSubscriptionReq>,
) -> ApiResult<Json<ChannelSubscriptionView>> {
    require_channel(&st, &id).await?;
    let mode = SubscriptionMode::parse(&req.mode)
        .ok_or_else(|| AppError::bad_request("unknown channel subscription mode"))?;
    let subject = match req
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    {
        None => principal_subject(&principal),
        Some(target) => {
            if let Grant::Session { session_id, .. } = &principal.grant {
                if !super::auth::is_session_descendant(&st, session_id, target).await {
                    return Err(AppError::new(
                        StatusCode::FORBIDDEN,
                        "a session may subscribe only itself or a descendant",
                    ));
                }
            } else if !principal.is_human() {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "credential may not subscribe another session",
                ));
            }
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?)")
                    .bind(target)
                    .fetch_one(&st.db)
                    .await?;
            if !exists {
                return Err(AppError::not_found("session"));
            }
            Subject::new(SubjectKind::Session, target)
        }
    };
    Ok(Json(
        channels::set_subscription(&st.db, &id, &subject, mode).await?,
    ))
}

pub(super) async fn set_channel_read_marker(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<SetChannelReadMarkerReq>,
) -> ApiResult<Json<ChannelSubscriptionView>> {
    require_channel(&st, &id).await?;
    let subject = principal_subject(&principal);
    Ok(Json(
        channels::mark_read(&st.db, &id, &subject, req.seq).await?,
    ))
}

pub(super) async fn archive_channel(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let channel = require_channel(&st, &id).await?;
    if channel.session_id.is_some() {
        return Err(AppError::conflict(
            "a session channel follows the session lifecycle",
        ));
    }
    let subject = principal_subject(&principal);
    if !principal.is_human()
        && (channel.created_by_kind != subject.kind.as_str() || channel.created_by != subject.id)
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "only the channel creator may archive it",
        ));
    }
    if !channels::archive_custom(&st.db, &id).await? {
        return Err(AppError::conflict("channel is already archived"));
    }
    events::record_system(
        &st.db,
        &st.bus,
        "channel_archived",
        json!({ "channel_id": id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "archived": true })))
}

async fn require_channel(st: &AppState, id: &str) -> ApiResult<channels::ChannelAccess> {
    channels::access(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("channel"))
}

fn validate_text(name: &str, value: &str, min: usize, max: usize) -> ApiResult<()> {
    let len = value.chars().count();
    if len < min || len > max {
        return Err(AppError::bad_request(format!(
            "{name} must be between {min} and {max} characters"
        )));
    }
    Ok(())
}
