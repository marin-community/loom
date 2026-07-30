//! Durable user/agent communication contexts.
//!
//! A session's default channel is inserted in the same transaction as the
//! session row. Messages are append-only and read state lives on per-subject
//! subscriptions; runtime delivery is a separate receipt.

use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::{FromRow, Row, SqliteConnection};
use weaver_api::{ChannelDeliveryView, ChannelMessageView, ChannelSubscriptionView, ChannelView};

use crate::db::{now_iso, Db};
use crate::session::{NewSession, SessionLaunchPolicy};

pub const SESSION_KIND: &str = "session";
pub const CUSTOM_KIND: &str = "custom";
pub const OPEN_STATE: &str = "open";
pub const ARCHIVED_STATE: &str = "archived";

pub const OBSERVE_MODE: &str = weaver_api::CHANNEL_DEFAULT_SUBSCRIPTION_MODE;
pub const DELIVER_MODE: &str = "deliver";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Goal,
    Message,
    Status,
    Result,
    System,
}

impl MessageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Message => weaver_api::CHANNEL_DEFAULT_MESSAGE_KIND,
            Self::Status => "status",
            Self::Result => "result",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "goal" => Some(Self::Goal),
            "message" => Some(Self::Message),
            "status" => Some(Self::Status),
            "result" => Some(Self::Result),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    Attention,
    Blocked,
}

impl Urgency {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => weaver_api::CHANNEL_DEFAULT_URGENCY,
            Self::Attention => "attention",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "attention" => Some(Self::Attention),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    Session,
    User,
    Automation,
    System,
}

impl SubjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::User => "user",
            Self::Automation => "automation",
            Self::System => "system",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "session" => Ok(Self::Session),
            "user" => Ok(Self::User),
            "automation" => Ok(Self::Automation),
            "system" => Ok(Self::System),
            _ => Err(anyhow!("unknown channel subject kind '{value}'")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: String,
}

impl Subject {
    pub fn new(kind: SubjectKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    fn from_parts(kind: &str, id: impl Into<String>) -> Result<Self> {
        Ok(Self::new(SubjectKind::parse(kind)?, id))
    }
}

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
    target_session_id: String,
    state: String,
    attempts: i64,
    last_error: Option<String>,
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
    pub session_id: Option<String>,
    pub state: String,
    pub created_by_kind: String,
    pub created_by: String,
}

/// Insert a new session's channel, opening goal message, and initial
/// memberships while the caller still owns the session insertion transaction.
pub(crate) async fn insert_session_channel_tx(
    tx: &mut SqliteConnection,
    session: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<()> {
    let row = sqlx::query("SELECT repo_root, title, goal FROM branches WHERE id = ?")
        .bind(&session.branch_id)
        .fetch_one(&mut *tx)
        .await?;
    let repo_root: String = row.get("repo_root");
    let title: String = row.get("title");
    let goal: String = row.get("goal");
    let now = now_iso();
    sqlx::query(
        "INSERT INTO channels
         (id, kind, repo_root, branch_id, session_id, name, topic, state,
          created_by_kind, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session.id)
    .bind(SESSION_KIND)
    .bind(repo_root)
    .bind(&session.branch_id)
    .bind(&session.id)
    .bind(if title.trim().is_empty() {
        &session.id
    } else {
        title.trim()
    })
    .bind(&goal)
    .bind(OPEN_STATE)
    .bind(&policy.creator_kind)
    .bind(&policy.creator_subject)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    let goal_seq = if goal.trim().is_empty() {
        0
    } else {
        sqlx::query(
            "INSERT INTO channel_messages
             (id, channel_id, seq, kind, urgency, author_kind, author_id,
              body, payload, created_at)
             VALUES (?, ?, 1, ?, ?, ?, ?, ?, '{}', ?)",
        )
        .bind(weaver_core::branch::new_id())
        .bind(&session.id)
        .bind(MessageKind::Goal.as_str())
        .bind(Urgency::Normal.as_str())
        .bind(&policy.creator_kind)
        .bind(&policy.creator_subject)
        .bind(goal)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        1
    };

    upsert_subscription_tx(
        tx,
        &session.id,
        &Subject::new(SubjectKind::Session, &session.id),
        Some(DELIVER_MODE),
        Some(goal_seq),
        &now,
    )
    .await?;
    upsert_subscription_tx(
        tx,
        &session.id,
        &Subject::from_parts(&policy.creator_kind, &policy.creator_subject)?,
        Some(OBSERVE_MODE),
        Some(goal_seq),
        &now,
    )
    .await?;
    if let Some(username) = session.created_by.as_deref() {
        upsert_subscription_tx(
            tx,
            &session.id,
            &Subject::new(SubjectKind::User, username),
            Some(OBSERVE_MODE),
            Some(goal_seq),
            &now,
        )
        .await?;
    }
    if let Some(parent) = policy.parent_session_id.as_deref() {
        upsert_subscription_tx(
            tx,
            &session.id,
            &Subject::new(SubjectKind::Session, parent),
            Some(OBSERVE_MODE),
            Some(goal_seq),
            &now,
        )
        .await?;
    }
    Ok(())
}

async fn upsert_subscription_tx(
    tx: &mut SqliteConnection,
    channel_id: &str,
    subject: &Subject,
    mode: Option<&str>,
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
    .bind(mode.unwrap_or(OBSERVE_MODE))
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
    upsert_subscription_tx(&mut tx, &id, creator, Some(OBSERVE_MODE), Some(0), &now).await?;
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
        "SELECT session_id, state, created_by_kind, created_by
         FROM channels WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| ChannelAccess {
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
        "SELECT target_session_id, state, attempts, last_error, updated_at
         FROM channel_deliveries WHERE message_id = ? ORDER BY target_session_id",
    )
    .bind(&row.id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|delivery| ChannelDeliveryView {
        target_session_id: delivery.target_session_id,
        state: delivery.state,
        attempts: delivery.attempts,
        last_error: delivery.last_error,
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
        payload: serde_json::from_str(&row.payload).unwrap_or(Value::Null),
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
            advance_read_tx(&mut tx, channel_id, new.author, row.seq, &read_at).await?;
            tx.commit().await?;
            return Ok(AppendOutcome {
                message: message_view(db, row).await?,
                inserted: false,
            });
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
    advance_read_tx(&mut tx, channel_id, new.author, seq, &now).await?;
    let row = sqlx::query_as::<_, MessageRow>("SELECT * FROM channel_messages WHERE id = ?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(AppendOutcome {
        message: message_view(db, row).await?,
        inserted: true,
    })
}

pub async fn set_subscription(
    db: &Db,
    channel_id: &str,
    subject: &Subject,
    mode: &str,
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

pub async fn create_delivery(db: &Db, message_id: &str, session_id: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO channel_deliveries
         (message_id, target_session_id, state, attempts, updated_at)
         VALUES (?, ?, 'queued', 0, ?)
         ON CONFLICT(message_id, target_session_id) DO NOTHING",
    )
    .bind(message_id)
    .bind(session_id)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn finish_delivery(
    db: &Db,
    message_id: &str,
    session_id: &str,
    error: Option<&str>,
) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "UPDATE channel_deliveries
         SET state = ?, attempts = attempts + 1, last_error = ?, updated_at = ?
         WHERE message_id = ? AND target_session_id = ?",
    )
    .bind(if error.is_some() {
        "failed"
    } else {
        "delivered"
    })
    .bind(error)
    .bind(now)
    .bind(message_id)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
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

        let subscription = mark_read(&db, "session-1", &owner, None).await.unwrap();
        assert_eq!(subscription.mode, DELIVER_MODE);
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
        set_subscription(&db, &custom.id, &owner, DELIVER_MODE)
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
    }
}
