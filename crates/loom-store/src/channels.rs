//! Durable user/agent communication contexts.
//!
//! A session's default channel is inserted in the same transaction as the
//! session row. Messages are append-only and read state lives on per-subject
//! subscriptions; runtime delivery is a separate receipt.

use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::{FromRow, Row, SqliteConnection};
use weaver_api::{ChannelDeliveryView, ChannelMessageView, ChannelSubscriptionView, ChannelView};

pub use crate::channel_data::{
    MessageKind, Subject, SubjectKind, SubscriptionMode, Urgency, ARCHIVED_STATE, CUSTOM_KIND,
    OPEN_STATE, SESSION_KIND,
};
use crate::db::{now_iso, Db};

#[derive(Debug, Clone, FromRow)]
struct ChannelRow {
    id: String,
    kind: String,
    repo_root: String,
    branch_id: Option<String>,
    session_id: Option<String>,
    name: String,
    topic: String,
    state: String,
    created_by_kind: String,
    created_by: String,
    created_at: String,
    archived_at: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
struct MessageRow {
    id: String,
    channel_id: String,
    seq: i64,
    kind: String,
    urgency: String,
    author_kind: String,
    author_id: String,
    body: String,
    payload: String,
    reply_to: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct DeliveryRow {
    binding_id: String,
    binding_kind: String,
    target_session_id: Option<String>,
    state: String,
    attempts: i64,
    last_error: Option<String>,
    external_id: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct SubscriptionRow {
    channel_id: String,
    subject_kind: String,
    subject_id: String,
    mode: String,
    read_seq: i64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ChannelAccess {
    pub branch_id: Option<String>,
    pub session_id: Option<String>,
    pub state: String,
    pub created_by_kind: String,
    pub created_by: String,
}

async fn upsert_subscription_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    subject: &Subject,
    mode: Option<SubscriptionMode>,
    read_seq: Option<i64>,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO channel_subscriptions
         (channel_id, subject_kind, subject_id, mode, read_seq, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(channel_id, subject_kind, subject_id) DO UPDATE SET
           mode = CASE WHEN ? THEN excluded.mode ELSE channel_subscriptions.mode END,
           read_seq = MAX(channel_subscriptions.read_seq, excluded.read_seq),
           updated_at = excluded.updated_at",
    )
    .bind(channel_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .bind(mode.unwrap_or(SubscriptionMode::Observe).as_str())
    .bind(read_seq.unwrap_or(0))
    .bind(now)
    .bind(now)
    .bind(mode.is_some())
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn advance_read_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    subject: &Subject,
    seq: i64,
    now: &str,
) -> Result<()> {
    upsert_subscription_tx(tx, channel_id, subject, None, Some(seq), now).await
}

pub async fn create_custom(
    db: &Db,
    repo_root: &str,
    branch_id: Option<&str>,
    name: &str,
    topic: &str,
    creator: &Subject,
) -> Result<ChannelView> {
    let id = weaver_core::branch::new_id();
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    sqlx::query(
        "INSERT INTO channels
         (id, kind, repo_root, branch_id, name, topic, state,
          created_by_kind, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(CUSTOM_KIND)
    .bind(repo_root)
    .bind(branch_id)
    .bind(name)
    .bind(topic)
    .bind(OPEN_STATE)
    .bind(creator.kind.as_str())
    .bind(&creator.id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    upsert_subscription_tx(
        &mut tx,
        &id,
        creator,
        Some(SubscriptionMode::Observe),
        Some(0),
        &now,
    )
    .await?;
    tx.commit().await?;
    get(db, &id, creator)
        .await?
        .ok_or_else(|| anyhow!("channel vanished after insert"))
}

pub async fn get(db: &Db, id: &str, subject: &Subject) -> Result<Option<ChannelView>> {
    let row = sqlx::query_as::<_, ChannelRow>("SELECT * FROM channels WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    match row {
        Some(row) => Ok(Some(channel_view(db, row, subject).await?)),
        None => Ok(None),
    }
}

pub async fn access(db: &Db, id: &str) -> Result<Option<ChannelAccess>> {
    let row = sqlx::query(
        "SELECT branch_id, session_id, state, created_by_kind, created_by
         FROM channels WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| ChannelAccess {
        branch_id: row.get("branch_id"),
        session_id: row.get("session_id"),
        state: row.get("state"),
        created_by_kind: row.get("created_by_kind"),
        created_by: row.get("created_by"),
    }))
}

pub async fn list_all(db: &Db, subject: &Subject, archived: bool) -> Result<Vec<ChannelView>> {
    let rows = sqlx::query_as::<_, ChannelRow>(
        "SELECT c.*
         FROM channels c
         LEFT JOIN sessions s ON s.id = c.session_id
         WHERE (c.session_id IS NULL OR s.managed_by IS NULL)
           AND (? OR c.state = 'open')
         ORDER BY c.created_at DESC",
    )
    .bind(archived)
    .fetch_all(db)
    .await?;
    views(db, rows, subject).await
}

pub async fn list_for_session_tree(
    db: &Db,
    root_session_id: &str,
    subject: &Subject,
    archived: bool,
) -> Result<Vec<ChannelView>> {
    let rows = sqlx::query_as::<_, ChannelRow>(
        "WITH RECURSIVE tree(id) AS (
           SELECT ?
           UNION ALL
           SELECT child.id
           FROM sessions child JOIN tree ON child.parent_session_id = tree.id
         )
         SELECT DISTINCT c.*
         FROM channels c
         LEFT JOIN channel_subscriptions sub
           ON sub.channel_id = c.id
          AND sub.subject_kind = ?
          AND sub.subject_id = ?
         WHERE (c.session_id IN (SELECT id FROM tree) OR sub.channel_id IS NOT NULL)
           AND (? OR c.state = 'open')
         ORDER BY c.created_at DESC",
    )
    .bind(root_session_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .bind(archived)
    .fetch_all(db)
    .await?;
    views(db, rows, subject).await
}

async fn views(db: &Db, rows: Vec<ChannelRow>, subject: &Subject) -> Result<Vec<ChannelView>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(channel_view(db, row, subject).await?);
    }
    Ok(out)
}

async fn channel_view(db: &Db, row: ChannelRow, subject: &Subject) -> Result<ChannelView> {
    let read_seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE((
           SELECT read_seq FROM channel_subscriptions
           WHERE channel_id = ? AND subject_kind = ? AND subject_id = ?
         ), 0)",
    )
    .bind(&row.id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .fetch_one(db)
    .await?;
    let unread_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_messages
         WHERE channel_id = ? AND seq > ?
           AND NOT (kind = 'goal' AND seq = 1)",
    )
    .bind(&row.id)
    .bind(read_seq)
    .fetch_one(db)
    .await?;
    let unread_urgent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_messages
         WHERE channel_id = ? AND seq > ? AND urgency IN ('attention', 'blocked')",
    )
    .bind(&row.id)
    .bind(read_seq)
    .fetch_one(db)
    .await?;
    let last_message = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM channel_messages WHERE channel_id = ? ORDER BY seq DESC LIMIT 1",
    )
    .bind(&row.id)
    .fetch_optional(db)
    .await?;
    Ok(ChannelView {
        id: row.id,
        kind: row.kind,
        repo_root: row.repo_root,
        branch_id: row.branch_id,
        session_id: row.session_id,
        name: row.name,
        topic: row.topic,
        state: row.state,
        created_by_kind: row.created_by_kind,
        created_by: row.created_by,
        created_at: row.created_at,
        archived_at: row.archived_at,
        unread_count,
        unread_urgent_count,
        last_message: match last_message {
            Some(message) => Some(message_view(db, message).await?),
            None => None,
        },
        // Populated by the server handler, which knows how to resolve
        // delivery bindings (session targets, the Slack origin thread); this
        // row-mapper only has the channel's own columns.
        bindings: Vec::new(),
    })
}

pub async fn messages(db: &Db, channel_id: &str, after: i64) -> Result<Vec<ChannelMessageView>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM channel_messages
         WHERE channel_id = ? AND seq > ? ORDER BY seq ASC",
    )
    .bind(channel_id)
    .bind(after)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(message_view(db, row).await?);
    }
    Ok(out)
}

async fn message_view(db: &Db, row: MessageRow) -> Result<ChannelMessageView> {
    let deliveries = sqlx::query_as::<_, DeliveryRow>(
        "SELECT binding_id, binding_kind, target_session_id, state, attempts,
                last_error, external_id, updated_at
         FROM channel_deliveries WHERE message_id = ? ORDER BY binding_id",
    )
    .bind(&row.id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|delivery| ChannelDeliveryView {
        binding_id: delivery.binding_id,
        binding_kind: delivery.binding_kind,
        target_session_id: delivery.target_session_id,
        state: delivery.state,
        attempts: delivery.attempts,
        last_error: delivery.last_error,
        external_id: delivery.external_id,
        updated_at: delivery.updated_at,
    })
    .collect();
    Ok(ChannelMessageView {
        id: row.id,
        channel_id: row.channel_id,
        seq: row.seq,
        kind: row.kind,
        urgency: row.urgency,
        author_kind: row.author_kind,
        author_id: row.author_id,
        body: row.body,
        payload: serde_json::from_str(&row.payload)?,
        reply_to: row.reply_to,
        created_at: row.created_at,
        deliveries,
    })
}

#[derive(Debug)]
pub struct NewMessage<'a> {
    pub kind: MessageKind,
    pub urgency: Urgency,
    pub author: &'a Subject,
    pub body: &'a str,
    pub payload: &'a Value,
    pub reply_to: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
}

pub async fn append(db: &Db, channel_id: &str, new: NewMessage<'_>) -> Result<ChannelMessageView> {
    Ok(append_with_outcome(db, channel_id, new).await?.message)
}

pub struct AppendOutcome {
    pub message: ChannelMessageView,
    pub inserted: bool,
}

pub async fn append_with_outcome(
    db: &Db,
    channel_id: &str,
    new: NewMessage<'_>,
) -> Result<AppendOutcome> {
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let (row, inserted) = append_message_tx(&mut tx, channel_id, &new).await?;
    if inserted {
        enqueue_message_tx(&mut tx, &row).await?;
        if new.kind == MessageKind::Result
            && new.author.kind == SubjectKind::Session
            && channel_id == new.author.id
        {
            let parent: Option<String> = sqlx::query_scalar(
                "SELECT c.id FROM sessions s JOIN channels c ON c.id = s.parent_session_id
                 WHERE s.id = ? AND c.state = 'open'",
            )
            .bind(&new.author.id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(parent) = parent {
                let excerpt: String = new.body.chars().take(2_000).collect();
                let truncated = if excerpt.len() < new.body.len() {
                    "\n[Result truncated]"
                } else {
                    ""
                };
                let body = format!(
                    "Result\n\n{excerpt}{truncated}\n\nSource: channel {channel_id}, result {}.\nRead the full result with `loom channels read --channel {channel_id} --after {} --limit 1 --kinds result`.",
                    row.id, row.seq - 1,
                );
                let body = crate::conversation_inbox::session_update_text(&new.author.id, &body);
                let payload = serde_json::json!({
                    "child_session_id": new.author.id,
                    "source_channel_id": channel_id,
                    "source_message_id": row.id,
                });
                let author = Subject::new(SubjectKind::System, "loom");
                let key = format!("child-result:{}", row.id);
                let (notice, _) = append_message_tx(
                    &mut tx,
                    &parent,
                    &NewMessage {
                        kind: MessageKind::Message,
                        urgency: new.urgency,
                        author: &author,
                        body: &body,
                        payload: &payload,
                        reply_to: None,
                        idempotency_key: Some(&key),
                    },
                )
                .await?;
                enqueue_message_tx(&mut tx, &notice).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(AppendOutcome {
        message: message_view(db, row).await?,
        inserted,
    })
}

async fn append_message_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    new: &NewMessage<'_>,
) -> Result<(MessageRow, bool)> {
    if let Some(key) = new.idempotency_key {
        if let Some(row) = sqlx::query_as::<_, MessageRow>(
            "SELECT * FROM channel_messages
             WHERE channel_id = ? AND idempotency_key = ?",
        )
        .bind(channel_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?
        {
            let read_at = now_iso();
            advance_read_tx(tx, channel_id, new.author, row.seq, &read_at).await?;
            return Ok((row, false));
        }
    }
    let seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM channel_messages WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_one(&mut *tx)
    .await?;
    let id = weaver_core::branch::new_id();
    let now = now_iso();
    sqlx::query(
        "INSERT INTO channel_messages
         (id, channel_id, seq, kind, urgency, author_kind, author_id, body,
          payload, reply_to, idempotency_key, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(channel_id)
    .bind(seq)
    .bind(new.kind.as_str())
    .bind(new.urgency.as_str())
    .bind(new.author.kind.as_str())
    .bind(&new.author.id)
    .bind(new.body)
    .bind(serde_json::to_string(new.payload)?)
    .bind(new.reply_to)
    .bind(new.idempotency_key)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    advance_read_tx(tx, channel_id, new.author, seq, &now).await?;
    let row = sqlx::query_as::<_, MessageRow>("SELECT * FROM channel_messages WHERE id = ?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    Ok((row, true))
}

async fn enqueue_message_tx(tx: &mut SqliteConnection, row: &MessageRow) -> Result<()> {
    if row.kind != "message" {
        return Ok(());
    }
    let targets: Vec<String> = sqlx::query_scalar(
        "SELECT sub.subject_id FROM channel_subscriptions sub JOIN sessions s ON s.id = sub.subject_id
         WHERE sub.channel_id = ? AND sub.subject_kind = 'session' AND sub.mode = 'deliver'
           AND NOT (? = 'session' AND sub.subject_id = ?)",
    ).bind(&row.channel_id).bind(&row.author_kind).bind(&row.author_id).fetch_all(&mut *tx).await?;
    let payload: Value = serde_json::from_str(&row.payload)?;
    let source_session = if row.author_kind == "session" {
        Some(row.author_id.as_str())
    } else if row.author_kind == "system" {
        payload.get("child_session_id").and_then(Value::as_str)
    } else {
        None
    };
    let source_channel = if row.author_kind == "system" {
        payload.get("source_channel_id").and_then(Value::as_str)
    } else {
        None
    }
    .unwrap_or(&row.channel_id);
    let source_message = if row.author_kind == "system" {
        payload.get("source_message_id").and_then(Value::as_str)
    } else {
        None
    }
    .unwrap_or(&row.id);
    let text = if row.author_kind == "session" {
        crate::conversation_inbox::session_update_text(&row.author_id, &row.body)
    } else {
        row.body.clone()
    };
    for target in targets {
        // A child result wakes its immediate parent, not every session that
        // happens to subscribe to the parent's channel.
        if row.author_kind == "system" && source_session.is_some() && target != row.channel_id {
            continue;
        }
        let binding = weaver_api::channel_session_binding_id(&target);
        sqlx::query("INSERT INTO channel_deliveries (message_id, binding_id, binding_kind, target_session_id, updated_at) VALUES (?, ?, 'session', ?, ?)")
            .bind(&row.id).bind(&binding).bind(&target).bind(now_iso()).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO conversation_inbox (delivery_key, branch_id, preferred_session_id, payload, source_session_id, source_channel_id, source_message_id)
             SELECT ?, branch_id, id, ?, ?, ?, ? FROM sessions WHERE id = ? AND protocol = 'acp'",
        ).bind(format!("channel:{}:{target}", row.id)).bind(&text).bind(source_session)
            .bind(source_channel).bind(source_message).bind(&target).execute(&mut *tx).await?;
    }
    Ok(())
}

/// Return the parent notice committed alongside this result, if one was created.
pub async fn parent_result_notice(db: &Db, result_id: &str) -> Result<Option<ChannelMessageView>> {
    let row = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM channel_messages WHERE author_kind = 'system' AND idempotency_key = ?",
    )
    .bind(format!("child-result:{result_id}"))
    .fetch_optional(db)
    .await?;
    match row {
        Some(row) => Ok(Some(message_view(db, row).await?)),
        None => Ok(None),
    }
}

pub async fn set_subscription(
    db: &Db,
    channel_id: &str,
    subject: &Subject,
    mode: SubscriptionMode,
) -> Result<ChannelSubscriptionView> {
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    upsert_subscription_tx(&mut tx, channel_id, subject, Some(mode), None, &now).await?;
    let row = sqlx::query_as::<_, SubscriptionRow>(
        "SELECT * FROM channel_subscriptions
         WHERE channel_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(channel_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(subscription_view(row))
}

pub async fn mark_read(
    db: &Db,
    channel_id: &str,
    subject: &Subject,
    requested_seq: Option<i64>,
) -> Result<ChannelSubscriptionView> {
    let max_seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) FROM channel_messages WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_one(db)
    .await?;
    let seq = requested_seq.unwrap_or(max_seq).clamp(0, max_seq);
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    // Advancing a read marker must not silently downgrade a `deliver`
    // subscription to `observe`. The default only applies when this is the
    // subject's first interaction with the channel.
    advance_read_tx(&mut tx, channel_id, subject, seq, &now).await?;
    let row = sqlx::query_as::<_, SubscriptionRow>(
        "SELECT * FROM channel_subscriptions
         WHERE channel_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(channel_id)
    .bind(subject.kind.as_str())
    .bind(&subject.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(subscription_view(row))
}

fn subscription_view(row: SubscriptionRow) -> ChannelSubscriptionView {
    ChannelSubscriptionView {
        channel_id: row.channel_id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        mode: row.mode,
        read_seq: row.read_seq,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

pub async fn create_delivery(
    db: &Db,
    message_id: &str,
    binding_id: &str,
    binding_kind: &str,
    target_session_id: Option<&str>,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO channel_deliveries
         (message_id, binding_id, binding_kind, target_session_id, state, attempts, updated_at)
         VALUES (?, ?, ?, ?, 'queued', 0, ?)
         ON CONFLICT(message_id, binding_id) DO NOTHING",
    )
    .bind(message_id)
    .bind(binding_id)
    .bind(binding_kind)
    .bind(target_session_id)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn finish_delivery(
    db: &Db,
    message_id: &str,
    binding_id: &str,
    error: Option<&str>,
    external_id: Option<&str>,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE channel_deliveries
         SET state = ?, attempts = attempts + 1, last_error = ?, external_id = ?, updated_at = ?
         WHERE message_id = ? AND binding_id = ?",
    )
    .bind(if error.is_some() {
        "failed"
    } else {
        "delivered"
    })
    .bind(error)
    .bind(external_id)
    .bind(now)
    .bind(message_id)
    .bind(binding_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delivery_succeeded(db: &Db, message_id: &str, binding_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM channel_deliveries
         WHERE message_id = ? AND binding_id = ? AND state IN ('delivered', 'read'))",
    )
    .bind(message_id)
    .bind(binding_id)
    .fetch_one(db)
    .await?)
}

/// Claim a terminal send before crossing the unacknowledged PTY boundary.
/// An uncertain send requires an explicit new publication, never a blind retry.
pub async fn claim_terminal_delivery(db: &Db, message: &str, binding: &str) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE channel_deliveries SET state = 'uncertain', attempts = attempts + 1,
             last_error = 'terminal send started without a confirmed receipt', updated_at = ?
         WHERE message_id = ? AND binding_id = ? AND state IN ('queued', 'failed')",
    )
    .bind(now_iso())
    .bind(message)
    .bind(binding)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

pub async fn finish_terminal_delivery(
    db: &Db,
    message: &str,
    binding: &str,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE channel_deliveries SET state = ?, last_error = ?, updated_at = ?
         WHERE message_id = ? AND binding_id = ? AND state = 'uncertain'",
    )
    .bind(if error.is_some() {
        "uncertain"
    } else {
        "delivered"
    })
    .bind(error)
    .bind(now_iso())
    .bind(message)
    .bind(binding)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn refresh_message(db: &Db, message_id: &str) -> Result<ChannelMessageView> {
    let row = sqlx::query_as::<_, MessageRow>("SELECT * FROM channel_messages WHERE id = ?")
        .bind(message_id)
        .fetch_one(db)
        .await?;
    message_view(db, row).await
}

pub async fn delivery_targets(db: &Db, channel_id: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT sub.subject_id
         FROM channel_subscriptions sub
         JOIN sessions s ON s.id = sub.subject_id
         WHERE sub.channel_id = ?
           AND sub.subject_kind = 'session'
           AND sub.mode = 'deliver'
         ORDER BY sub.subject_id",
    )
    .bind(channel_id)
    .fetch_all(db)
    .await?)
}

pub async fn message_delivery_targets(db: &Db, message: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT target_session_id FROM channel_deliveries WHERE message_id = ? AND binding_kind = 'session' AND target_session_id IS NOT NULL",
    ).bind(message).fetch_all(db).await?)
}

pub async fn archive_session_channel(db: &Db, session_id: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE channels SET state = ?, archived_at = ?
         WHERE session_id = ? AND state != ?",
    )
    .bind(ARCHIVED_STATE)
    .bind(now)
    .bind(session_id)
    .bind(ARCHIVED_STATE)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn reopen_session_channel(db: &Db, session_id: &str) -> Result<()> {
    let changed = sqlx::query(
        "UPDATE channels SET state = ?, archived_at = NULL
         WHERE session_id = ? AND state = ?",
    )
    .bind(OPEN_STATE)
    .bind(session_id)
    .bind(ARCHIVED_STATE)
    .execute(db)
    .await?;
    if changed.rows_affected() > 0 {
        let author = Subject::new(SubjectKind::System, "loom");
        append(
            db,
            session_id,
            NewMessage {
                kind: MessageKind::System,
                urgency: Urgency::Normal,
                author: &author,
                body: "session recovered",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await?;
    }
    Ok(())
}

pub async fn archive_custom(db: &Db, channel_id: &str) -> Result<bool> {
    let now = now_iso();
    let result = sqlx::query(
        "UPDATE channels SET state = ?, archived_at = ?
         WHERE id = ? AND kind = ? AND state != ?",
    )
    .bind(ARCHIVED_STATE)
    .bind(now)
    .bind(channel_id)
    .bind(CUSTOM_KIND)
    .bind(ARCHIVED_STATE)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_session_goal(db: &Db, session_id: &str, goal: &str) -> Result<()> {
    let state: Option<String> =
        sqlx::query_scalar("UPDATE channels SET topic = ? WHERE session_id = ? RETURNING state")
            .bind(goal)
            .bind(session_id)
            .fetch_optional(db)
            .await?;
    if state.as_deref() != Some(OPEN_STATE) {
        return Ok(());
    }
    let author = Subject::new(SubjectKind::User, "manual");
    append(
        db,
        session_id,
        NewMessage {
            kind: MessageKind::Goal,
            urgency: Urgency::Normal,
            author: &author,
            body: if goal.trim().is_empty() {
                "(goal cleared)"
            } else {
                goal
            },
            payload: &serde_json::json!({ "updated": true, "goal": goal }),
            reply_to: None,
            idempotency_key: None,
        },
    )
    .await?;
    Ok(())
}

pub async fn update_branch_channel_names(db: &Db, branch_id: &str, name: &str) -> Result<()> {
    sqlx::query(
        "UPDATE channels SET name = ?
         WHERE branch_id = ? AND session_id IS NOT NULL",
    )
    .bind(name)
    .bind(branch_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn session_channel_for_branch(db: &Db, branch_id: &str) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT c.id
         FROM channels c
         JOIN sessions s ON s.id = c.session_id
         WHERE c.branch_id = ? AND c.state = 'open'
         ORDER BY CASE WHEN s.status IN ('done', 'error', 'archived') THEN 1 ELSE 0 END,
                  s.created_at DESC
         LIMIT 1",
    )
    .bind(branch_id)
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{self, NewSession};
    use weaver_core::branch as branch_mod;

    fn new_session(id: &str, branch_id: &str) -> NewSession {
        NewSession {
            id: id.to_string(),
            branch_id: branch_id.to_string(),
            work_dir: format!("/work/{id}"),
            term_session: format!("weaver-{id}"),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: Some("alice".to_string()),
            protocol: "terminal".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        }
    }

    #[tokio::test]
    async fn session_insert_creates_goal_channel_and_preserves_delivery_mode_on_read() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_mod::upsert(&db, "/repo", "weaver/channels", "main")
            .await
            .unwrap();
        branch_mod::set_title(
            &db,
            &branch.id,
            "Channel work",
            branch_mod::TitleProvenance::User,
        )
        .await
        .unwrap();
        branch_mod::set_goal(&db, &branch.id, "Build durable channels", "user")
            .await
            .unwrap();
        session::insert(&db, &new_session("session-1", &branch.id))
            .await
            .unwrap();

        let owner = Subject::new(SubjectKind::Session, "session-1");
        let user = Subject::new(SubjectKind::User, "alice");
        let channel = get(&db, "session-1", &user).await.unwrap().unwrap();
        assert_eq!(channel.kind, SESSION_KIND);
        assert_eq!(channel.name, "Channel work");
        assert_eq!(channel.topic, "Build durable channels");
        assert_eq!(channel.unread_count, 0);

        update_branch_channel_names(&db, &branch.id, "Renamed channel")
            .await
            .unwrap();
        assert_eq!(
            get(&db, "session-1", &user).await.unwrap().unwrap().name,
            "Renamed channel"
        );

        let opening = messages(&db, "session-1", 0).await.unwrap();
        assert_eq!(opening.len(), 1);
        assert_eq!(opening[0].kind, MessageKind::Goal.as_str());
        assert_eq!(opening[0].body, "Build durable channels");

        update_session_goal(&db, "session-1", "Ship the channel API")
            .await
            .unwrap();
        let updated = get(&db, "session-1", &user).await.unwrap().unwrap();
        assert_eq!(updated.topic, "Ship the channel API");
        let goal_updates = messages(&db, "session-1", 1).await.unwrap();
        assert_eq!(goal_updates.len(), 1);
        assert_eq!(goal_updates[0].kind, MessageKind::Goal.as_str());
        assert_eq!(goal_updates[0].body, "Ship the channel API");

        let posted = append(
            &db,
            "session-1",
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Attention,
                author: &user,
                body: "Please check the API boundary",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: Some("request-1"),
            },
        )
        .await
        .unwrap();
        let replay = append(
            &db,
            "session-1",
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Attention,
                author: &user,
                body: "Please check the API boundary",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: Some("request-1"),
            },
        )
        .await
        .unwrap();
        assert_eq!(posted.id, replay.id, "idempotent replay appends once");

        let binding = weaver_api::channel_session_binding_id("session-1");
        assert!(claim_terminal_delivery(&db, &posted.id, &binding)
            .await
            .unwrap());
        assert!(!claim_terminal_delivery(&db, &posted.id, &binding)
            .await
            .unwrap());
        finish_terminal_delivery(&db, &posted.id, &binding, Some("receipt lost"))
            .await
            .unwrap();
        assert!(
            !claim_terminal_delivery(&db, &posted.id, &binding)
                .await
                .unwrap(),
            "uncertain terminal writes are not blindly replayed"
        );

        create_delivery(
            &db,
            &posted.id,
            weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID,
            "slack_thread",
            None,
        )
        .await
        .unwrap();
        finish_delivery(
            &db,
            &posted.id,
            weaver_api::CHANNEL_SLACK_ORIGIN_BINDING_ID,
            None,
            Some("1786.1234"),
        )
        .await
        .unwrap();
        let delivered = refresh_message(&db, &posted.id).await.unwrap();
        assert_eq!(delivered.deliveries.len(), 2);
        let slack = delivered
            .deliveries
            .iter()
            .find(|delivery| delivery.binding_kind == "slack_thread")
            .unwrap();
        assert_eq!(slack.target_session_id, None);
        assert_eq!(slack.external_id.as_deref(), Some("1786.1234"));

        let subscription = mark_read(&db, "session-1", &owner, None).await.unwrap();
        assert_eq!(subscription.mode, SubscriptionMode::Deliver.as_str());
        assert_eq!(subscription.read_seq, posted.seq);

        let custom = create_custom(
            &db,
            "/repo",
            Some(&branch.id),
            "Review room",
            "Explicit monitor",
            &owner,
        )
        .await
        .unwrap();
        set_subscription(&db, &custom.id, &owner, SubscriptionMode::Deliver)
            .await
            .unwrap();
        assert_eq!(
            delivery_targets(&db, &custom.id).await.unwrap(),
            vec!["session-1"],
            "custom channels deliver only after an explicit session subscription"
        );

        session::delete(&db, "session-1").await.unwrap();
        branch_mod::delete(&db, &branch.id).await.unwrap();
        assert!(
            get(&db, &custom.id, &owner).await.unwrap().is_some(),
            "a custom channel outlives its creator's session branch"
        );
        let later = append(
            &db,
            &custom.id,
            NewMessage {
                kind: MessageKind::Message,
                urgency: Urgency::Normal,
                author: &user,
                body: "still usable",
                payload: &Value::Null,
                reply_to: None,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
        assert!(
            later.deliveries.is_empty(),
            "deleted subscribers do not block publication"
        );
    }

    #[tokio::test]
    async fn result_publication_commits_notice_and_inbox_once_or_rolls_back_all_three() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_mod::upsert(&db, "/repo", "weaver/parent", "main")
            .await
            .unwrap();
        let mut parent = new_session("parent", &branch.id);
        parent.protocol = "acp".into();
        session::insert(&db, &parent).await.unwrap();
        let child_branch = branch_mod::upsert(&db, "/repo", "weaver/child", "main")
            .await
            .unwrap();
        session::insert(&db, &new_session("child", &child_branch.id))
            .await
            .unwrap();
        sqlx::query("UPDATE sessions SET parent_session_id = 'parent' WHERE id = 'child'")
            .execute(&db)
            .await
            .unwrap();
        let child = Subject::new(SubjectKind::Session, "child");
        let body = "✓".repeat(2_001);
        let send = || {
            append_with_outcome(
                &db,
                "child",
                NewMessage {
                    kind: MessageKind::Result,
                    urgency: Urgency::Normal,
                    author: &child,
                    body: &body,
                    payload: &Value::Null,
                    reply_to: None,
                    idempotency_key: Some("result:1"),
                },
            )
        };
        sqlx::query("CREATE TRIGGER fail_inbox BEFORE INSERT ON conversation_inbox BEGIN SELECT RAISE(ABORT, 'injected inbox failure'); END").execute(&db).await.unwrap();
        assert!(send().await.is_err());
        assert!(messages(&db, "child", 0).await.unwrap().is_empty());
        assert!(messages(&db, "parent", 0).await.unwrap().is_empty());
        sqlx::query("DROP TRIGGER fail_inbox")
            .execute(&db)
            .await
            .unwrap();
        let (first, retry) = tokio::join!(send(), send());
        let first = first.unwrap();
        assert_eq!(first.message.id, retry.unwrap().message.id);
        let notices = messages(&db, "parent", 0).await.unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].body.matches('✓').count(), 2_000);
        let updates = crate::conversation_inbox::pending_updates(&db, "parent")
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].source_message_id.as_deref(),
            Some(first.message.id.as_str())
        );
        crate::conversation_inbox::consume_source(&db, "parent", &first.message.id)
            .await
            .unwrap();
        assert!(crate::conversation_inbox::pending_updates(&db, "parent")
            .await
            .unwrap()
            .is_empty());
        send().await.unwrap();
        assert!(crate::conversation_inbox::pending_updates(&db, "parent")
            .await
            .unwrap()
            .is_empty());
    }
}
