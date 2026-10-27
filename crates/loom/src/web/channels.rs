use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::json;
use weaver_api::operations::channels as ops;
use weaver_api::{
    ChannelArchiveResult, ChannelBindingView, ChannelMessageView, ChannelSubscriptionView,
    ChannelView, SendReq,
};

use crate::{
    auth::{Grant, Principal},
    channels::{self, MessageKind, NewMessage, Subject, SubjectKind, SubscriptionMode, Urgency},
    events,
};

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

const MAX_NAME_LEN: usize = 120;
const MAX_TOPIC_LEN: usize = 4_096;
const MAX_BODY_LEN: usize = 256 * 1024;

pub(super) fn principal_subject(principal: &Principal) -> Subject {
    match &principal.grant {
        Grant::Session { session_id, .. } => Subject::new(SubjectKind::Session, session_id),
        Grant::Automation { subject, .. } => Subject::new(SubjectKind::Automation, subject),
        // `Anonymous` reaches nothing that authors or reads a channel — every
        // `channels.*` operation declares an actor policy that excludes it
        // (see `operations::actor_allows`) — but the match must stay total.
        Grant::Admin | Grant::User | Grant::Anonymous => {
            Subject::new(SubjectKind::User, &principal.username)
        }
    }
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
    req: &ops::messages::create::Input,
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
    let message_id = outcome.message.id.clone();
    let body = outcome.message.body.as_str();

    // Session channels are durable inboxes; custom channels become inboxes for
    // agents that explicitly subscribe in `deliver` mode. Only ordinary
    // conversation reaches runtimes, and an agent never prompts itself.
    // A plain message carries its own wake; a result is woken by the parent
    // notice path below, so the delivery pass does not double-wake for it.
    if kind == MessageKind::Message {
        deliver_to_subscribers(st, author, &message_id, body, true).await?;
    }

    // A session-authored message or result on its own channel is the canonical
    // reply stream. A Slack-origin binding fans it out, but the durable channel
    // item and idempotency key are what's authoritative.
    if matches!(kind, MessageKind::Message | MessageKind::Result)
        && author.kind == SubjectKind::Session
        && channel.session_id.as_deref() == Some(author.id.as_str())
    {
        deliver_to_origin_slack(st, channel, &message_id, body).await?;
        if kind == MessageKind::Result {
            if let Err(error) = notify_parent_of_result(st, &outcome.message, inserted).await {
                // The child's channel remains the canonical result even when a
                // parent has been archived or its notification cannot be sent.
                tracing::warn!(child = %author.id, message = %message_id, error = %error.message(),
                    "could not notify parent of child result");
            }
        }
    }

    Ok((
        inserted,
        channels::refresh_message(&st.db, &message_id).await?,
    ))
}

async fn deliver_to_subscribers(
    st: &AppState,
    author: &Subject,
    message_id: &str,
    body: &str,
    wake_dormant: bool,
) -> ApiResult<()> {
    for target in channels::message_delivery_targets(&st.db, message_id).await? {
        let binding_id = weaver_api::channel_session_binding_id(&target);
        if channels::delivery_succeeded(&st.db, message_id, &binding_id).await? {
            continue;
        }
        let session = crate::session::get(&st.db, &target)
            .await?
            .ok_or_else(|| AppError::not_found("session"))?;
        if session.protocol == "acp" {
            // Publication already committed the protected inbox entry. A wake
            // is only a hint; the task also checks this queue after recovery.
            if wake_dormant
                && (session.lifecycle_transition.as_deref() == Some("suspending")
                    || crate::session::is_suspended(&session.status))
            {
                // Dormant — or a suspend still committing. The entry must not
                // depend on the (possibly already-stopping) live driver
                // dispatching it in time: schedule the detached wake, whose
                // helper waits out the settle window on its own budget and
                // wakes the session, whose inbox drain then lands the entry.
                // A live driver is still nudged — it may deliver before its
                // stop, and the scheduled wake covers the case where it
                // does not.
                let st_for_wake = st.clone();
                let target_for_wake = target.clone();
                tokio::spawn(async move {
                    crate::lifecycle::wake_dormant_session(
                        &st_for_wake,
                        &target_for_wake,
                        "channel delivery",
                        false,
                    )
                    .await;
                });
                if let Some(handle) = st.acp.get(&target) {
                    if let Err(error) = handle.notify_queue().await {
                        tracing::debug!(session = %target, %error, "session update remains queued");
                    }
                }
                continue;
            }
            if let Some(handle) = st.acp.get(&target) {
                if let Err(error) = handle.notify_queue().await {
                    tracing::debug!(session = %target, %error, "session update remains queued");
                }
            }
            // The notify raced a suspend that claimed the row after the
            // read above: the entry is durable in the inbox, but nothing
            // would wake the parked row (the inbox drain only runs once a
            // task exists) and the live driver may stop before it
            // dispatches. Schedule the wake, whose helper waits out the
            // settle window; a plain message is its own wake signal.
            if wake_dormant {
                if let Ok(Some(fresh)) = crate::session::get(&st.db, &target).await {
                    if crate::session::is_suspended(&fresh.status)
                        || fresh.lifecycle_transition.as_deref() == Some("suspending")
                    {
                        let st_for_wake = st.clone();
                        let target_for_wake = target.clone();
                        tokio::spawn(async move {
                            crate::lifecycle::wake_dormant_session(
                                &st_for_wake,
                                &target_for_wake,
                                "channel delivery",
                                false,
                            )
                            .await;
                        });
                    }
                }
            }
            continue;
        }
        // The terminal lane is deliberately dumb: a clean running row is
        // pasted into inline, anything else — dormant, mid-transition — is
        // skipped with the message left durable in channel history. A
        // suspended terminal session never queues and never auto-wakes:
        // every direct ingress refuses with "wake it first", and the
        // sender that gets that error (or the operator reading the
        // channel's unread count) wakes the session and retries.
        let text = if author.kind == SubjectKind::Session {
            crate::conversation_inbox::session_update_text(&author.id, body)
        } else {
            body.to_string()
        };
        let by = format!("channel:{}", author.id);
        if session.status != "running" || session.lifecycle_transition.is_some() {
            tracing::debug!(session = %target,
                "terminal delivery skipped: the session is not drivable; the message is durable in channel history");
            continue;
        }
        paste_terminal_delivery(st, &target, message_id, &binding_id, &text, &by).await;
    }
    Ok(())
}

/// Paste one channel message into a cleanly running terminal session and
/// settle its delivery receipt. The one place the terminal lane touches a
/// pane: no claims, no retry queue — a failure is recorded and the
/// re-publication of the same message is the retry.
async fn paste_terminal_delivery(
    st: &AppState,
    target: &str,
    message_id: &str,
    binding_id: &str,
    text: &str,
    by: &str,
) {
    let failure = super::sessions::send_session(
        State(st.clone()),
        Path(target.to_string()),
        Json(SendReq {
            text: text.to_string(),
            submit: true,
            by: Some(by.to_string()),
        }),
    )
    .await
    .err();
    let error = failure.map(|error| error.message().to_string());
    if let Err(settle_error) =
        channels::settle_terminal_delivery(&st.db, message_id, binding_id, error.as_deref()).await
    {
        tracing::warn!(session = %target, %settle_error,
            "could not settle the terminal delivery receipt");
    }
    if let Some(error) = error {
        tracing::warn!(session = %target, %error,
            "terminal channel delivery failed; a re-publication of the message is the retry");
    }
}

async fn notify_parent_of_result(
    st: &AppState,
    result: &ChannelMessageView,
    inserted: bool,
) -> ApiResult<()> {
    let Some(notice) = channels::parent_result_notice(&st.db, &result.id).await? else {
        return Ok(());
    };
    let notice_author = Subject::new(SubjectKind::System, "loom");
    // The notice's own delivery must not spawn a second wake — the insert
    // below owns waking the parent, with surfacing on.
    deliver_to_subscribers(st, &notice_author, &notice.id, &notice.body, false).await?;
    // A committed result is a wake signal for a dormant ACP parent: the
    // notice is durable in its protected inbox, and the respawned task's
    // inbox drain is what makes it land. The wake is guarded on `inserted`
    // — a duplicate post must not re-wake — with one carve-out: if the
    // notice is still sitting `queued` in the parent's inbox, the original
    // wake was lost (a process can die between the insert and this spawn),
    // so a re-post revives rather than idles. Detached so the child's own
    // request never waits out the parent's respawn; the wake helper
    // retries transient refusals and, if the retries run out, raises the
    // parent's triage tag — a result that cannot reach its consumer must
    // not sit quietly in a dormant row.
    //
    // A dormant TERMINAL parent never wakes on a result: the terminal lane
    // has no queue and no auto-wake, so the notice stays in channel
    // history and the parent's triage tag is raised — the operator wakes
    // it, and the unread result is waiting on the channel.
    if let Some(parent) = crate::session::get(&st.db, &notice.channel_id).await? {
        // Covers the mid-`suspending` race too: the helper waits out a
        // suspend that is still committing, then wakes the committed row.
        let dormant_or_parking = crate::session::is_suspended(&parent.status)
            || parent.lifecycle_transition.as_deref() == Some("suspending");
        let wake = if parent.protocol != "acp" {
            if dormant_or_parking {
                crate::lifecycle::raise_attention(
                    st,
                    &parent,
                    &format!(
                        "a committed result is waiting on the dormant terminal parent (child result {})",
                        result.id
                    ),
                )
                .await;
            }
            false
        } else if inserted {
            dormant_or_parking
        } else {
            // A duplicate post only re-wakes a result that never landed:
            // the notice is pending in the parent's inbox lane, which
            // settles only when the delivery actually happens.
            dormant_or_parking
                && crate::conversation_inbox::delivery_state(
                    &st.db,
                    &format!("channel:{}:{}", notice.id, parent.id),
                )
                .await
                .ok()
                .flatten()
                .is_some_and(|state| state == "queued")
        };
        if wake {
            let st = st.clone();
            let parent_id = parent.id.clone();
            let result_id = result.id.clone();
            tokio::spawn(async move {
                crate::lifecycle::wake_dormant_session(
                    &st,
                    &parent_id,
                    &format!("child result {result_id}"),
                    true,
                )
                .await;
            });
        }
    }
    record_channel_message_event(st, &notice.channel_id, &notice_author, &notice, inserted).await;
    Ok(())
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

/// Resolve a channel and confirm the caller may reach it.
///
/// Called by `scope_allows` for every `scope = Channel` operation, so no handler
/// can forget it. Answered before the channel is looked up, and a refusal is 403
/// whether or not the channel exists — a session that may not reach a channel
/// learns nothing about whether it is there.
pub(super) async fn session_may_reach(
    st: &AppState,
    principal: &Principal,
    channel: &str,
) -> ApiResult<bool> {
    let id = resolve_channel_id(principal, channel)?;
    let Grant::Session { session_id, .. } = &principal.grant else {
        return Ok(true);
    };
    Ok(channel_belongs_to_session_tree(st, session_id, &id).await)
}

/// The channel row, or 404.
async fn channel_access(st: &AppState, id: &str) -> ApiResult<channels::ChannelAccess> {
    channels::access(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("channel"))
}

/// A channel is in a session's tree if the session is subscribed to it, or if
/// the channel is the session channel of the session itself or one of its
/// descendants.
///
/// Enforces the rule `scope = Branch` cannot express: operation paths carry no
/// channel id, so nothing upstream of the handler knows which channel a
/// request is about. This function is that check.
async fn channel_belongs_to_session_tree(st: &AppState, ancestor: &str, channel_id: &str) -> bool {
    let row = sqlx::query(
        "SELECT c.session_id,
                EXISTS(
                  SELECT 1 FROM channel_subscriptions sub
                  WHERE sub.channel_id = c.id
                    AND sub.subject_kind = 'session'
                    AND sub.subject_id = ?
                ) AS subscribed
         FROM channels c WHERE c.id = ?",
    )
    .bind(ancestor)
    .bind(channel_id)
    .fetch_optional(&st.db)
    .await;
    let Ok(Some(row)) = row else {
        return false;
    };
    if sqlx::Row::get::<bool, _>(&row, "subscribed") {
        return true;
    }
    match sqlx::Row::get::<Option<String>, _>(&row, "session_id") {
        Some(session_id) => super::auth::is_session_descendant(st, ancestor, &session_id).await,
        None => false,
    }
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

// ---------------------------------------------------------------------------
// Operation registry bindings
//
// Every channel operation is handled via the typed registry bindings registered
// below. Authorization (actor policy, grants, and branch scope from `Scoped`)
// happens once in `register`/`authorize`; a manual check only remains where it
// polices something the declared `Branch` scope cannot see, such as which *session*
// a caller may act as a proxy for.
// ---------------------------------------------------------------------------

/// Resolve the `channel` operand all eight operations share: an explicit id
/// (or the legacy `"self"` alias some MCP callers still send), or — when
/// omitted — the calling session's own channel (its id equals the session
/// id, the same resolution `GET /api/self` performs for `channel_id`). A
/// credential with no session of its own (`User`/`Admin`) has no implicit
/// channel and must name one.
pub(super) fn resolve_channel_id(principal: &Principal, channel: &str) -> ApiResult<String> {
    let trimmed = channel.trim();
    if !trimmed.is_empty() && trimmed != "self" {
        return Ok(trimmed.to_string());
    }
    match &principal.grant {
        Grant::Session { session_id, .. } => Ok(session_id.clone()),
        _ => Err(AppError::bad_request(
            "channel is required for a credential with no session of its own",
        )),
    }
}

/// Fill in a [`ChannelView`]'s delivery bindings. The row mapper in
/// `loom-store` leaves `bindings` empty because only the server handler knows
/// how to resolve delivery targets and the Slack origin thread.
/// `channels.get` and `channels.list` resolve and return them.
async fn with_bindings(st: &AppState, mut view: ChannelView) -> ApiResult<ChannelView> {
    if let Some(access) = channels::access(&st.db, &view.id).await? {
        view.bindings = channel_bindings(st, &view.id, &access).await?;
    }
    Ok(view)
}

pub(super) async fn list_channels_operation(
    context: OperationContext,
    input: ops::list::Input,
) -> ApiResult<Vec<ChannelView>> {
    let st = context.state;
    let principal = context.principal;
    let subject = principal_subject(&principal);
    let channels = match &principal.grant {
        Grant::Session { session_id, .. } => {
            channels::list_for_session_tree(&st.db, session_id, &subject, input.archived).await?
        }
        _ => channels::list_all(&st.db, &subject, input.archived).await?,
    };
    let mut out = Vec::with_capacity(channels.len());
    for channel in channels {
        out.push(with_bindings(&st, channel).await?);
    }
    Ok(out)
}

pub(super) async fn get_channel_operation(
    context: OperationContext,
    input: ops::get::Input,
) -> ApiResult<ChannelView> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    channel_access(&st, &channel_id).await?;
    let subject = principal_subject(&principal);
    let channel = channels::get(&st.db, &channel_id, &subject)
        .await?
        .ok_or_else(|| AppError::not_found("channel"))?;
    with_bindings(&st, channel).await
}

/// Read a channel's history and, unless `peek`, advance the read marker
/// through the last item actually returned (not the full unbounded scan) —
/// advancing past a `kinds`-filtered or `limit`-truncated tail would mark
/// items the caller never saw as read.
pub(super) async fn list_channel_messages_operation(
    context: OperationContext,
    input: ops::messages::list::Input,
) -> ApiResult<Vec<ChannelMessageView>> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    channel_access(&st, &channel_id).await?;
    if !(1..=weaver_api::CHANNEL_MESSAGE_LIMIT_MAX as i64).contains(&input.limit) {
        return Err(AppError::bad_request(format!(
            "limit must be between 1 and {}",
            weaver_api::CHANNEL_MESSAGE_LIMIT_MAX
        )));
    }
    let mut messages = channels::messages(&st.db, &channel_id, input.after.max(0)).await?;
    if !input.kinds.is_empty() {
        messages.retain(|message| input.kinds.iter().any(|kind| kind == &message.kind));
    }
    messages.truncate(input.limit as usize);
    if !input.peek {
        if let Some(last) = messages.last() {
            let subject = principal_subject(&principal);
            channels::mark_read(&st.db, &channel_id, &subject, Some(last.seq)).await?;
            if subject.kind == SubjectKind::Session {
                for message in &messages {
                    crate::conversation_inbox::consume_source(&st.db, &subject.id, &message.id)
                        .await?;
                }
                if let Some(handle) = st.acp.get(&subject.id) {
                    if let Err(error) = handle.notify_queue().await {
                        tracing::debug!(%error, "channel read consumed pending updates; refresh deferred");
                    }
                }
            }
        }
    }
    Ok(messages)
}

pub(super) async fn create_channel_message_operation(
    context: OperationContext,
    input: ops::messages::create::Input,
) -> ApiResult<ChannelMessageView> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    let channel = channel_access(&st, &channel_id).await?;
    let author = principal_subject(&principal);
    let (inserted, message) =
        append_and_deliver(&st, &channel_id, &channel, &author, &input).await?;
    record_channel_message_event(&st, &channel_id, &author, &message, inserted).await;
    Ok(message)
}

/// `channels.create` — open a custom channel in a repository.
///
/// The channel belongs to `repo_root`; `branch` only records which branch
/// opened it. Both are context operands: a session gets them from its own row,
/// a human from the repo and branch they name.
pub(super) async fn create_channel_operation(
    context: OperationContext,
    input: ops::create::Input,
) -> ApiResult<ChannelView> {
    let st = context.state;
    let principal = context.principal;
    let name = input.name.trim();
    let topic = input.topic.trim();
    validate_text("name", name, 1, MAX_NAME_LEN)?;
    validate_text("topic", topic, 0, MAX_TOPIC_LEN)?;
    let subject = principal_subject(&principal);
    let channel = channels::create_custom(
        &st.db,
        &input.repo_root,
        input.branch.as_deref(),
        name,
        topic,
        &subject,
    )
    .await?;
    events::record_system(
        &st.db,
        &st.bus,
        "channel_created",
        json!({ "channel_id": channel.id, "by": subject.id }),
    )
    .await
    .ok();
    Ok(channel)
}

/// `channels.archive` — retire a custom channel.
///
/// Who may archive is narrower than who may reach the channel, which is why the
/// creator check stays here: `scope = Branch` gets the caller as far as the
/// channel, and this decides whether it is theirs to close. A session channel is
/// refused outright — it follows the session's lifecycle, not a caller's.
pub(super) async fn archive_channel_operation(
    context: OperationContext,
    input: ops::archive::Input,
) -> ApiResult<ChannelArchiveResult> {
    let st = context.state;
    let principal = context.principal;
    let id = input.channel;
    let channel = channel_access(&st, &id).await?;
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
    Ok(ChannelArchiveResult { archived: true })
}

pub(super) async fn set_channel_subscription_operation(
    context: OperationContext,
    input: ops::subscription::set::Input,
) -> ApiResult<ChannelSubscriptionView> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    channel_access(&st, &channel_id).await?;
    let mode = SubscriptionMode::parse(&input.mode)
        .ok_or_else(|| AppError::bad_request("unknown channel subscription mode"))?;
    // Not a duplicate of the central branch-scope check: `Scoped` only names
    // this operation's own branch, so it says nothing about which *session*
    // a caller may subscribe on behalf of. That authority question is
    // answered here.
    let subject = match input
        .session
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
    Ok(channels::set_subscription(&st.db, &channel_id, &subject, mode).await?)
}

pub(super) async fn set_channel_read_marker_operation(
    context: OperationContext,
    input: ops::read_marker::set::Input,
) -> ApiResult<ChannelSubscriptionView> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    channel_access(&st, &channel_id).await?;
    let subject = principal_subject(&principal);
    Ok(channels::mark_read(&st.db, &channel_id, &subject, input.seq).await?)
}

/// Long-poll for the next channel message matching `kind`/`urgent`. Defaults
/// the scan cursor to the channel's latest known message, validates `timeout`
/// to the `1..=3600` second window, then polls once a second until a match
/// lands or the deadline passes. Returns exactly one response after the wait,
/// never a stream.
pub(super) async fn wait_for_channel_message_operation(
    context: OperationContext,
    input: ops::wait::Input,
) -> ApiResult<ChannelMessageView> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    channel_access(&st, &channel_id).await?;
    let subject = principal_subject(&principal);
    let channel = channels::get(&st.db, &channel_id, &subject)
        .await?
        .ok_or_else(|| AppError::not_found("channel"))?;
    let mut cursor = input.after.unwrap_or_else(|| {
        channel
            .last_message
            .as_ref()
            .map(|message| message.seq)
            .unwrap_or(0)
    });
    if cursor < 0 {
        return Err(AppError::bad_request("after must be non-negative"));
    }
    if !(1..=3600).contains(&input.timeout) {
        return Err(AppError::bad_request(
            "timeout must be between 1 and 3600 seconds",
        ));
    }
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(input.timeout as u64);
    loop {
        let messages = channels::messages(&st.db, &channel_id, cursor).await?;
        if let Some(last) = messages.last() {
            cursor = last.seq;
        }
        if let Some(message) = messages.into_iter().find(|message| {
            input
                .kind
                .as_deref()
                .is_none_or(|kind| message.kind == kind)
                && (!input.urgent || matches!(message.urgency.as_str(), "attention" | "blocked"))
        }) {
            if subject.kind == SubjectKind::Session {
                crate::conversation_inbox::consume_source(&st.db, &subject.id, &message.id).await?;
                if let Some(handle) = st.acp.get(&subject.id) {
                    if let Err(error) = handle.notify_queue().await {
                        tracing::debug!(%error, "result consumed; inbox refresh deferred");
                    }
                }
            }
            return Ok(message);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::new(
                StatusCode::REQUEST_TIMEOUT,
                format!("timed out waiting for channel {channel_id}"),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<ops::list::Op, _, _>(list_channels_operation),
        register::<ops::get::Op, _, _>(get_channel_operation),
        register::<ops::messages::list::Op, _, _>(list_channel_messages_operation),
        register::<ops::messages::create::Op, _, _>(create_channel_message_operation),
        register::<ops::create::Op, _, _>(create_channel_operation),
        register::<ops::archive::Op, _, _>(archive_channel_operation),
        register::<ops::subscription::set::Op, _, _>(set_channel_subscription_operation),
        register::<ops::read_marker::set::Op, _, _>(set_channel_read_marker_operation),
        register::<ops::wait::Op, _, _>(wait_for_channel_message_operation),
        register::<ops::bindings::list::Op, _, _>(list_channel_bindings_operation),
    ]
}

pub(super) async fn list_channel_bindings_operation(
    context: OperationContext,
    input: ops::bindings::list::Input,
) -> ApiResult<Vec<ChannelBindingView>> {
    let st = context.state;
    let principal = context.principal;
    let channel_id = resolve_channel_id(&principal, &input.channel)?;
    let channel = channel_access(&st, &channel_id).await?;
    channel_bindings(&st, &channel_id, &channel).await
}
