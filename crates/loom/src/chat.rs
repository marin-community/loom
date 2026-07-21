//! The chat journal for ACP sessions — the durable, block-structured record of a
//! session's conversation that [`crate::acp`] writes and the `/chat` REST routes
//! read.
//!
//! One row per consolidated *block*, addressed by `(session_id, turn, seq)`:
//! `turn` is the 0-based prompt cycle, `seq` the 0-based position within it.
//! Every write is idempotent — `INSERT OR IGNORE` on that key — so a relay spool
//! replay after a loom restart re-ingests the same frames without duplicating a
//! block. The one mutable block is `permission_request`: inserted open, then
//! `UPDATE`d in place with its outcome when resolved (keyed by the upstream
//! request id inside the payload).
//!
//! `payload` is opaque JSON here; its shape is keyed by `kind` (see the block
//! kinds documented on [`crate::acp`]). This module only stores and reads it.

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::db::{now_iso, Db};

/// Block kinds. The set is closed; [`crate::acp`] maps ACP `session/update`
/// variants onto these.
pub mod kind {
    pub const USER_MESSAGE: &str = "user_message";
    pub const AGENT_MESSAGE: &str = "agent_message";
    pub const THOUGHT: &str = "thought";
    pub const TOOL_CALL: &str = "tool_call";
    pub const PLAN: &str = "plan";
    pub const PERMISSION_REQUEST: &str = "permission_request";
    pub const MODE_CHANGE: &str = "mode_change";
    pub const USAGE: &str = "usage";
    pub const TURN_END: &str = "turn_end";
    pub const HANDOFF: &str = "handoff";
}

/// Build the provider-neutral bootstrap given to a replacement agent. Only the
/// authored dialogue is replayed: the worktree already carries tool effects,
/// while thoughts and raw tool output are noisy, provider-specific machinery.
/// Keep a bounded tail so a long-running session cannot consume the target's
/// whole context window before it starts useful work.
pub fn handoff_prompt(goal: &str, blocks: &[ChatBlockView], max_chars: usize) -> String {
    let mut dialogue = String::new();
    for block in blocks {
        let speaker = match block.kind.as_str() {
            kind::USER_MESSAGE => "User",
            kind::AGENT_MESSAGE => "Agent",
            _ => continue,
        };
        let text = block
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            continue;
        }
        dialogue.push_str(speaker);
        dialogue.push_str(":\n");
        dialogue.push_str(text);
        dialogue.push_str("\n\n");
    }

    let (dialogue, omitted) = tail_chars(&dialogue, max_chars);
    let omission = omitted.then_some(
        "Earlier dialogue was omitted to fit the handoff context; the canonical journal remains in loom.\n\n",
    );
    format!(
        "You are taking over an existing coding session from another agent provider. Continue the work in the current worktree; do not restart completed work.\n\nGoal:\n{}\n\nPrior conversation:\n{}{}",
        goal.trim(),
        omission.unwrap_or(""),
        dialogue.trim()
    )
}

fn tail_chars(text: &str, max_chars: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= max_chars {
        return (text.to_string(), false);
    }
    (
        text.chars().skip(count.saturating_sub(max_chars)).collect(),
        true,
    )
}

/// One journaled block as the `/chat` routes expose it. `payload` is passed
/// through as JSON — the client renders it by `kind`.
#[derive(Debug, Clone, Serialize)]
pub struct ChatBlockView {
    pub turn: i64,
    pub seq: i64,
    pub kind: String,
    pub payload: Value,
    pub created_at: String,
}

/// Insert a block idempotently. Returns `true` when the row was newly written,
/// `false` when `(session_id, turn, seq)` already existed (a replay). `payload`
/// is serialized to a JSON string for storage.
pub async fn insert(
    db: &Db,
    session_id: &str,
    turn: i64,
    seq: i64,
    kind: &str,
    payload: &Value,
) -> Result<bool> {
    let res = sqlx::query(
        "INSERT OR IGNORE INTO chat_blocks (session_id, turn, seq, kind, payload, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(turn)
    .bind(seq)
    .bind(kind)
    .bind(payload.to_string())
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Every block for a session, in `(turn, seq)` order — the transcript snapshot
/// the client fetches before tailing the SSE stream.
pub async fn list(db: &Db, session_id: &str) -> Result<Vec<ChatBlockView>> {
    let rows = sqlx::query(
        "SELECT turn, seq, kind, payload, created_at FROM chat_blocks
         WHERE session_id = ? ORDER BY turn ASC, seq ASC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(row_to_view).collect())
}

fn row_to_view(r: sqlx::sqlite::SqliteRow) -> ChatBlockView {
    ChatBlockView {
        turn: r.get("turn"),
        seq: r.get("seq"),
        kind: r.get("kind"),
        payload: serde_json::from_str(&r.get::<String, _>("payload")).unwrap_or(Value::Null),
        created_at: r.get("created_at"),
    }
}

/// The highest `(turn, seq)` present for a session, or `None` when the journal is
/// empty. Used on task (re)start to resume the block cursor without double-writing.
pub async fn max_turn_seq(db: &Db, session_id: &str) -> Result<Option<(i64, i64)>> {
    // The lexicographic max of (turn, seq): the max turn, then the max seq within it.
    let row = sqlx::query(
        "SELECT turn, seq FROM chat_blocks WHERE session_id = ?
         ORDER BY turn DESC, seq DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| (r.get("turn"), r.get("seq"))))
}

/// The `(turn, seq)` and payload of every still-open `permission_request` block
/// for a session — those whose payload `outcome` is JSON null. On task restart
/// [`crate::acp`] reloads these so a REST answer can still resolve one; the
/// matching un-acked frame replays the JSON-RPC id.
pub async fn open_permissions(db: &Db, session_id: &str) -> Result<Vec<ChatBlockView>> {
    let rows = sqlx::query(
        "SELECT turn, seq, kind, payload, created_at FROM chat_blocks
         WHERE session_id = ? AND kind = ? ORDER BY turn ASC, seq ASC",
    )
    .bind(session_id)
    .bind(kind::PERMISSION_REQUEST)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(row_to_view)
        .filter(|v| v.payload.get("outcome").map(Value::is_null).unwrap_or(true))
        .collect())
}

/// The journal's knowledge of a `permission_request`, looked up by its
/// upstream `request_id`.
#[derive(Debug, PartialEq, Eq)]
pub enum PermissionOutcome {
    /// No such request journaled.
    Unknown,
    /// Journaled, still awaiting an answer.
    Open,
    /// Answered with this option id.
    Resolved(String),
}

/// The current outcome of a `permission_request` identified by its upstream
/// `request_id`. Lets a replayed permission frame decide whether to re-send a
/// stored answer.
pub async fn permission_outcome(
    db: &Db,
    session_id: &str,
    request_id: &str,
) -> Result<PermissionOutcome> {
    let rows = sqlx::query("SELECT payload FROM chat_blocks WHERE session_id = ? AND kind = ?")
        .bind(session_id)
        .bind(kind::PERMISSION_REQUEST)
        .fetch_all(db)
        .await?;
    for r in rows {
        let payload: Value =
            serde_json::from_str(&r.get::<String, _>("payload")).unwrap_or(Value::Null);
        if payload.get("request_id").and_then(Value::as_str) == Some(request_id) {
            return Ok(match payload.get("outcome") {
                Some(Value::Null) | None => PermissionOutcome::Open,
                Some(o) => match o.get("option_id").and_then(Value::as_str) {
                    Some(id) => PermissionOutcome::Resolved(id.to_string()),
                    None => PermissionOutcome::Open,
                },
            });
        }
    }
    Ok(PermissionOutcome::Unknown)
}

/// Resolve an open `permission_request` in place: set its `outcome` to the chosen
/// option, author, and time. Idempotent — matches the block by `request_id` and
/// only touches a block whose outcome is still null, so a re-answer (or replay)
/// is a no-op. Returns the updated block on success, `None` when no open block
/// with that `request_id` exists.
pub async fn resolve_permission(
    db: &Db,
    session_id: &str,
    request_id: &str,
    option_id: &str,
    by: &str,
) -> Result<Option<ChatBlockView>> {
    let rows = sqlx::query(
        "SELECT turn, seq, kind, payload, created_at FROM chat_blocks
         WHERE session_id = ? AND kind = ?",
    )
    .bind(session_id)
    .bind(kind::PERMISSION_REQUEST)
    .fetch_all(db)
    .await?;
    for r in rows {
        let mut view = row_to_view(r);
        if view.payload.get("request_id").and_then(Value::as_str) != Some(request_id) {
            continue;
        }
        // Only an open block is resolvable; a resolved one is left untouched.
        if !view
            .payload
            .get("outcome")
            .map(Value::is_null)
            .unwrap_or(true)
        {
            return Ok(None);
        }
        if let Value::Object(map) = &mut view.payload {
            map.insert(
                "outcome".to_string(),
                serde_json::json!({ "option_id": option_id, "by": by, "at": now_iso() }),
            );
        }
        sqlx::query(
            "UPDATE chat_blocks SET payload = ? WHERE session_id = ? AND turn = ? AND seq = ?",
        )
        .bind(view.payload.to_string())
        .bind(session_id)
        .bind(view.turn)
        .bind(view.seq)
        .execute(db)
        .await?;
        return Ok(Some(view));
    }
    Ok(None)
}

/// Whether a `tool_call` block for `tool_call_id` is already journaled — the
/// idempotency check that keeps a replayed terminal tool-call frame from
/// re-journaling the block at a fresh seq (tool calls have no `(turn, seq)`
/// stability across a restart, but their upstream id is stable).
pub async fn tool_call_exists(db: &Db, session_id: &str, tool_call_id: &str) -> Result<bool> {
    let rows = sqlx::query("SELECT payload FROM chat_blocks WHERE session_id = ? AND kind = ?")
        .bind(session_id)
        .bind(kind::TOOL_CALL)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().any(|r| {
        serde_json::from_str::<Value>(&r.get::<String, _>("payload"))
            .ok()
            .and_then(|p| {
                p.get("tool_call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .as_deref()
            == Some(tool_call_id)
    }))
}

/// Whether a `turn_end` block is already journaled for `turn` — the idempotency
/// check that keeps a replayed prompt-response frame from re-journaling turn end.
pub async fn has_turn_end(db: &Db, session_id: &str, turn: i64) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_blocks WHERE session_id = ? AND turn = ? AND kind = ?",
    )
    .bind(session_id)
    .bind(turn)
    .bind(kind::TURN_END)
    .fetch_one(db)
    .await?;
    Ok(n > 0)
}

/// Close a turn abandoned by a vanished ACP task. The opening user block is
/// already durable before `acp_inflight` is written; this supplies the missing
/// terminal boundary before a replacement provider starts.
pub async fn close_abandoned_turn(db: &Db, session_id: &str, turn: i64) -> Result<()> {
    if has_turn_end(db, session_id, turn).await? {
        return Ok(());
    }
    let seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), -1) + 1 FROM chat_blocks
         WHERE session_id = ? AND turn = ?",
    )
    .bind(session_id)
    .bind(turn)
    .fetch_one(db)
    .await?;
    insert(
        db,
        session_id,
        turn,
        seq,
        kind::TURN_END,
        &json!({ "stop_reason": "error" }),
    )
    .await?;
    Ok(())
}

/// Append an internal context-usage reset at the journal tail. Handoff keeps
/// historical usage blocks, but current usage must read as unknown until the
/// replacement provider reports its own context window.
pub async fn reset_usage(db: &Db, session_id: &str) -> Result<()> {
    let (turn, seq) = match max_turn_seq(db, session_id).await? {
        Some((turn, seq)) => (turn, seq + 1),
        None => (0, 0),
    };
    insert(
        db,
        session_id,
        turn,
        seq,
        kind::USAGE,
        &json!({ "used": null, "size": null, "reset": true }),
    )
    .await?;
    Ok(())
}

/// Render the last `last_n` journal blocks as compact plain text — the ACP
/// analogue of the terminal `preview` screen (`[who] text` lines for prose, a
/// one-liner for the machine's apparatus). CLI convenience only.
pub fn preview_text(blocks: &[ChatBlockView], last_n: usize) -> String {
    let start = blocks.len().saturating_sub(last_n);
    let mut out = String::new();
    for b in &blocks[start..] {
        let line = preview_line(b);
        if !line.is_empty() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// One journal block as a single compact preview line (empty to skip it).
fn preview_line(b: &ChatBlockView) -> String {
    let p = &b.payload;
    let text = |key: &str| p.get(key).and_then(Value::as_str).unwrap_or("").trim();
    match b.kind.as_str() {
        kind::USER_MESSAGE => format!("[you] {}", text("text")),
        kind::AGENT_MESSAGE => format!("[agent] {}", text("text")),
        kind::THOUGHT => format!("[thinking] {}", text("text")),
        kind::TOOL_CALL => {
            let tool_kind = p.get("tool_kind").and_then(Value::as_str).unwrap_or("tool");
            let title = p.get("title").and_then(Value::as_str).unwrap_or("");
            let status = p.get("status").and_then(Value::as_str).unwrap_or("");
            format!("  · {tool_kind} {title} [{status}]")
        }
        kind::PLAN => {
            let n = p
                .get("entries")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            format!("[plan] {n} entries")
        }
        kind::PERMISSION_REQUEST => {
            let title = p.get("title").and_then(Value::as_str).unwrap_or("");
            let outcome = p
                .get("outcome")
                .and_then(|o| o.get("option_id"))
                .and_then(Value::as_str)
                .unwrap_or("pending");
            format!("[permission] {title} ({outcome})")
        }
        kind::MODE_CHANGE => format!("[mode] {}", text("mode_id")),
        kind::USAGE => match (
            p.get("used").and_then(Value::as_u64),
            p.get("size").and_then(Value::as_u64),
        ) {
            (Some(used), Some(size)) => format!("[usage] {used}/{size}"),
            _ => String::new(),
        },
        kind::TURN_END => {
            let reason = p.get("stop_reason").and_then(Value::as_str).unwrap_or("");
            format!("— turn {} · {reason} —", b.turn)
        }
        kind::HANDOFF => {
            let from = p.get("from").and_then(Value::as_str).unwrap_or("agent");
            let to = p.get("to").and_then(Value::as_str).unwrap_or("agent");
            format!("[handoff] {from} → {to}")
        }
        _ => String::new(),
    }
}

/// The latest `usage` block's payload for a session, or `None`. A provider
/// handoff appends a null marker, which intentionally parses as `None` until the
/// replacement reports its own context.
/// A cheap query feeding [`SessionView::usage`](weaver_api::SessionView).
pub async fn latest_usage(db: &Db, session_id: &str) -> Result<Option<weaver_api::AcpUsage>> {
    let row = sqlx::query(
        "SELECT payload FROM chat_blocks WHERE session_id = ? AND kind = ?
         ORDER BY turn DESC, seq DESC LIMIT 1",
    )
    .bind(session_id)
    .bind(kind::USAGE)
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|r| serde_json::from_str(&r.get::<String, _>("payload")).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn seed_session(db: &Db) -> String {
        let branch = weaver_core::branch::upsert(db, "/repo", "weaver/chat", "main")
            .await
            .unwrap();
        crate::session::insert(
            db,
            &crate::session::NewSession {
                id: "chatsess".to_string(),
                branch_id: branch.id,
                work_dir: "/w".to_string(),
                term_session: "weaver-chatsess".to_string(),
                agent_kind: "claude".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: None,
                protocol: "acp".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        "chatsess".to_string()
    }

    #[tokio::test]
    async fn insert_is_idempotent_on_turn_seq() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let s = seed_session(&db).await;
        assert!(insert(
            &db,
            &s,
            0,
            0,
            kind::USER_MESSAGE,
            &json!({"text":"hi","by":null})
        )
        .await
        .unwrap());
        // Same (turn, seq) again — a replay — is ignored, not duplicated.
        assert!(!insert(
            &db,
            &s,
            0,
            0,
            kind::USER_MESSAGE,
            &json!({"text":"hi","by":null})
        )
        .await
        .unwrap());
        let blocks = list(&db, &s).await.unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, kind::USER_MESSAGE);
    }

    #[tokio::test]
    async fn max_turn_seq_is_lexicographic() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let s = seed_session(&db).await;
        insert(&db, &s, 0, 0, kind::USER_MESSAGE, &json!({}))
            .await
            .unwrap();
        insert(&db, &s, 0, 5, kind::TURN_END, &json!({}))
            .await
            .unwrap();
        insert(&db, &s, 1, 2, kind::AGENT_MESSAGE, &json!({}))
            .await
            .unwrap();
        assert_eq!(max_turn_seq(&db, &s).await.unwrap(), Some((1, 2)));
    }

    #[tokio::test]
    async fn permission_resolution_is_idempotent_by_request_id() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let s = seed_session(&db).await;
        insert(
            &db,
            &s,
            0,
            1,
            kind::PERMISSION_REQUEST,
            &json!({ "request_id": "req-1", "tool_call_id": null, "title": "edit",
                     "options": [{"option_id":"allow","name":"Allow","kind":"allow_once"}],
                     "outcome": null }),
        )
        .await
        .unwrap();

        assert_eq!(open_permissions(&db, &s).await.unwrap().len(), 1);
        assert_eq!(
            permission_outcome(&db, &s, "req-1").await.unwrap(),
            PermissionOutcome::Open
        );
        assert_eq!(
            permission_outcome(&db, &s, "req-absent").await.unwrap(),
            PermissionOutcome::Unknown
        );

        let resolved = resolve_permission(&db, &s, "req-1", "allow", "alice")
            .await
            .unwrap()
            .expect("open request resolves");
        assert_eq!(resolved.payload["outcome"]["option_id"], "allow");
        assert_eq!(resolved.payload["outcome"]["by"], "alice");

        // No longer open; a second resolve is a no-op.
        assert!(open_permissions(&db, &s).await.unwrap().is_empty());
        assert!(resolve_permission(&db, &s, "req-1", "allow", "bob")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            permission_outcome(&db, &s, "req-1").await.unwrap(),
            PermissionOutcome::Resolved("allow".to_string())
        );
    }

    #[test]
    fn preview_text_renders_compact_lines_for_the_tail() {
        let block = |turn: i64, seq: i64, kind: &str, payload: Value| ChatBlockView {
            turn,
            seq,
            kind: kind.to_string(),
            payload,
            created_at: String::new(),
        };
        let blocks = vec![
            block(
                0,
                0,
                kind::USER_MESSAGE,
                json!({"text":"do the thing","by":null}),
            ),
            block(
                0,
                1,
                kind::TOOL_CALL,
                json!({"tool_kind":"edit","title":"file.rs","status":"completed"}),
            ),
            block(0, 2, kind::AGENT_MESSAGE, json!({"text":"done"})),
            block(0, 3, kind::TURN_END, json!({"stop_reason":"end_turn"})),
        ];
        // The whole tail.
        let all = preview_text(&blocks, 40);
        assert!(all.contains("[you] do the thing"), "{all}");
        assert!(all.contains("· edit file.rs [completed]"), "{all}");
        assert!(all.contains("[agent] done"), "{all}");
        assert!(all.contains("turn 0 · end_turn"), "{all}");
        // Only the last N.
        let tail = preview_text(&blocks, 1);
        assert!(tail.contains("end_turn"));
        assert!(!tail.contains("[you]"), "only the last block: {tail}");
    }

    #[test]
    fn preview_text_skips_usage_resets_and_malformed_usage() {
        let usage = |payload: Value| ChatBlockView {
            turn: 0,
            seq: 0,
            kind: kind::USAGE.to_string(),
            payload,
            created_at: String::new(),
        };
        let blocks = vec![
            usage(json!({"used":10,"size":100})),
            usage(json!({"used":null,"size":null,"reset":true})),
            usage(json!({"used":"unknown","size":100})),
        ];

        assert_eq!(preview_text(&blocks, blocks.len()), "[usage] 10/100\n");
    }

    #[tokio::test]
    async fn latest_usage_returns_the_newest() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let s = seed_session(&db).await;
        insert(&db, &s, 0, 3, kind::USAGE, &json!({"used":100,"size":200}))
            .await
            .unwrap();
        insert(&db, &s, 1, 4, kind::USAGE, &json!({"used":150,"size":200}))
            .await
            .unwrap();
        assert_eq!(latest_usage(&db, &s).await.unwrap().unwrap().used, 150);
        reset_usage(&db, &s).await.unwrap();
        assert_eq!(latest_usage(&db, &s).await.unwrap(), None);
    }

    #[tokio::test]
    async fn close_abandoned_turn_is_idempotent() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let s = seed_session(&db).await;
        insert(
            &db,
            &s,
            2,
            0,
            kind::USER_MESSAGE,
            &json!({"text":"unfinished"}),
        )
        .await
        .unwrap();
        close_abandoned_turn(&db, &s, 2).await.unwrap();
        close_abandoned_turn(&db, &s, 2).await.unwrap();
        assert!(has_turn_end(&db, &s, 2).await.unwrap());
        assert_eq!(
            list(&db, &s)
                .await
                .unwrap()
                .iter()
                .filter(|block| block.kind == kind::TURN_END)
                .count(),
            1
        );
    }

    #[test]
    fn handoff_prompt_replays_only_dialogue_and_bounds_the_tail() {
        let block = |kind: &str, text: &str| ChatBlockView {
            turn: 0,
            seq: 0,
            kind: kind.to_string(),
            payload: json!({"text": text}),
            created_at: String::new(),
        };
        let blocks = vec![
            block(kind::USER_MESSAGE, "old user context"),
            block(kind::THOUGHT, "private reasoning"),
            block(kind::AGENT_MESSAGE, "recent answer"),
            block(kind::TOOL_CALL, "large tool output"),
        ];
        let prompt = handoff_prompt("finish it", &blocks, 20);
        assert!(prompt.contains("Goal:\nfinish it"));
        assert!(prompt.contains("recent answer"));
        assert!(prompt.contains("Earlier dialogue was omitted"));
        assert!(!prompt.contains("private reasoning"));
        assert!(!prompt.contains("large tool output"));
    }
}
