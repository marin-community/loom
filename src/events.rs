//! Append-only event log plus an in-process broadcast channel for SSE.
//!
//! Durable events (`status`, `hook`, `summary`, `note`) are persisted with
//! [`record`]. High-volume transient events (`screen`) are only broadcast with
//! [`emit`] and never hit the database.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use tokio::sync::broadcast;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: i64,
    pub workspace_id: String,
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
    workspace_id: &str,
    kind: &str,
    data: Value,
) -> Result<Event> {
    let row = sqlx::query(
        "INSERT INTO events (workspace_id, kind, data) VALUES (?, ?, ?) RETURNING id, created_at",
    )
    .bind(workspace_id)
    .bind(kind)
    .bind(data.to_string())
    .fetch_one(db)
    .await?;
    let event = Event {
        id: row.get("id"),
        workspace_id: workspace_id.to_string(),
        kind: kind.to_string(),
        data,
        created_at: row.get("created_at"),
    };
    bus.publish(event.clone());
    Ok(event)
}

/// Broadcast a transient event without persisting it.
pub fn emit(bus: &EventBus, workspace_id: &str, kind: &str, data: Value) {
    bus.publish(Event {
        id: 0,
        workspace_id: workspace_id.to_string(),
        kind: kind.to_string(),
        data,
        created_at: now_iso(),
    });
}

/// Most recent persisted events for a workspace, oldest first.
pub async fn history(db: &Db, workspace_id: &str, limit: i64) -> Result<Vec<Event>> {
    let rows = sqlx::query(
        "SELECT id, workspace_id, kind, data, created_at FROM events
         WHERE workspace_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(workspace_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut events: Vec<Event> = rows
        .into_iter()
        .map(|r| Event {
            id: r.get("id"),
            workspace_id: r.get("workspace_id"),
            kind: r.get("kind"),
            data: serde_json::from_str(&r.get::<String, _>("data")).unwrap_or(Value::Null),
            created_at: r.get("created_at"),
        })
        .collect();
    events.reverse();
    Ok(events)
}
