//! Append-only event log plus an in-process broadcast channel for SSE.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use tokio::sync::broadcast;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: i64,
    pub branch_id: String,
    pub kind: String,
    pub data: Value,
    pub created_at: String,
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: Event) {
        // Err only means there are no subscribers; that is fine.
        let _ = self.tx.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Persist an event and broadcast it.
pub async fn record(
    db: &Db,
    bus: &EventBus,
    branch_id: &str,
    kind: &str,
    data: Value,
) -> Result<Event> {
    let row = sqlx::query(
        "INSERT INTO events (branch_id, kind, data) VALUES (?, ?, ?) RETURNING id, created_at",
    )
    .bind(branch_id)
    .bind(kind)
    .bind(data.to_string())
    .fetch_one(db)
    .await?;
    let event = Event {
        id: row.get("id"),
        branch_id: branch_id.to_string(),
        kind: kind.to_string(),
        data,
        created_at: row.get("created_at"),
    };
    tracing::debug!(branch_id, kind, id = event.id, "recorded event");
    bus.publish(event.clone());
    Ok(event)
}

/// Persist an event without going through a bus (the `weaver hook` path that
/// runs without a daemon).
pub async fn record_local(db: &Db, branch_id: &str, kind: &str, data: Value) -> Result<i64> {
    let row =
        sqlx::query("INSERT INTO events (branch_id, kind, data) VALUES (?, ?, ?) RETURNING id")
            .bind(branch_id)
            .bind(kind)
            .bind(data.to_string())
            .fetch_one(db)
            .await?;
    Ok(row.get("id"))
}

/// Broadcast a transient event without persisting it.
pub fn emit(bus: &EventBus, branch_id: &str, kind: &str, data: Value) {
    bus.publish(Event {
        id: 0,
        branch_id: branch_id.to_string(),
        kind: kind.to_string(),
        data,
        created_at: now_iso(),
    });
}

/// Most recent persisted events for a branch, oldest first.
pub async fn history(db: &Db, branch_id: &str, limit: i64) -> Result<Vec<Event>> {
    let rows = sqlx::query(
        "SELECT id, branch_id, kind, data, created_at FROM events
         WHERE branch_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(branch_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut events: Vec<Event> = rows
        .into_iter()
        .map(|r| Event {
            id: r.get("id"),
            branch_id: r.get("branch_id"),
            kind: r.get("kind"),
            data: serde_json::from_str(&r.get::<String, _>("data")).unwrap_or(Value::Null),
            created_at: r.get("created_at"),
        })
        .collect();
    events.reverse();
    Ok(events)
}

/// Fetch every event with id strictly greater than `since`, oldest first.
/// Used by the monitor to consume hook events written by the `weaver hook`
/// command.
pub async fn since(db: &Db, since: i64) -> Result<Vec<Event>> {
    let rows = sqlx::query(
        "SELECT id, branch_id, kind, data, created_at FROM events
         WHERE id > ? ORDER BY id ASC",
    )
    .bind(since)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Event {
            id: r.get("id"),
            branch_id: r.get("branch_id"),
            kind: r.get("kind"),
            data: serde_json::from_str(&r.get::<String, _>("data")).unwrap_or(Value::Null),
            created_at: r.get("created_at"),
        })
        .collect())
}

/// The highest event id currently in the table, or 0 when the table is empty.
pub async fn max_id(db: &Db) -> Result<i64> {
    let row: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM events")
        .fetch_optional(db)
        .await?
        .flatten();
    Ok(row.unwrap_or(0))
}
