use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{
    ChannelMessageView, ChannelSubscriptionView, ChannelView, CreateChannelMessageReq,
    CreateChannelReq, SendReq, SetChannelReadMarkerReq, SetChannelSubscriptionReq,
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
}

pub(super) fn principal_subject(principal: &Principal) -> Subject {
    match &principal.grant {
        Grant::Session { session_id, .. } => Subject::new(SubjectKind::Session, session_id),
        Grant::Automation { subject, .. } => Subject::new(SubjectKind::Automation, subject),
        Grant::Admin => Subject::new(SubjectKind::User, &principal.username),
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
    Ok(Json(
        channels::messages(&st.db, &id, query.after.max(0)).await?,
    ))
}

pub(super) async fn create_channel_message(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<CreateChannelMessageReq>,
) -> ApiResult<(StatusCode, Json<ChannelMessageView>)> {
    let channel = require_channel(&st, &id).await?;
    if channel.state != channels::OPEN_STATE {
        return Err(AppError::conflict("channel is archived"));
    }
    let kind = MessageKind::parse(&req.kind)
        .ok_or_else(|| AppError::bad_request("unknown channel message kind"))?;
    let urgency = Urgency::parse(&req.urgency)
        .ok_or_else(|| AppError::bad_request("unknown channel message urgency"))?;
    let body = req.body.trim();
    validate_text("body", body, 1, MAX_BODY_LEN)?;
    if let Some(reply_to) = req.reply_to.as_deref() {
        let belongs: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM channel_messages WHERE id = ? AND channel_id = ?
             )",
        )
        .bind(reply_to)
        .bind(&id)
        .fetch_one(&st.db)
        .await?;
        if !belongs {
            return Err(AppError::bad_request(
                "reply_to must identify a message in this channel",
            ));
        }
    }
    let author = principal_subject(&principal);
    let outcome = channels::append_with_outcome(
        &st.db,
        &id,
        NewMessage {
            kind,
            urgency,
            author: &author,
            body,
            payload: &req.payload,
            reply_to: req.reply_to.as_deref(),
            idempotency_key: req.idempotency_key.as_deref(),
        },
    )
    .await?;
    let inserted = outcome.inserted;
    let mut message = outcome.message;

    // Session channels are durable inboxes; custom channels become inboxes for
    // agents that explicitly subscribe in `deliver` mode. Only ordinary
    // conversation reaches runtimes, and an agent never prompts itself.
    if inserted && kind == MessageKind::Message {
        for target in channels::delivery_targets(&st.db, &id).await? {
            if author.kind == SubjectKind::Session && author.id == target {
                continue;
            }
            channels::create_delivery(&st.db, &message.id, &target).await?;
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
            channels::finish_delivery(&st.db, &message.id, &target, delivery_error.as_deref())
                .await?;
        }
        message = channels::messages(&st.db, &id, message.seq - 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::not_found("channel message"))?;
    }

    if inserted {
        events::record_system(
            &st.db,
            &st.bus,
            "channel_message",
            json!({
                "channel_id": id,
                "message_id": message.id,
                "kind": message.kind,
                "urgency": message.urgency,
                "by": author.id,
            }),
        )
        .await
        .ok();
    }
    Ok((
        if inserted {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(message),
    ))
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
            } else if !principal.is_admin() {
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
    if !principal.is_admin()
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
