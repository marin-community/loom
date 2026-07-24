//! Provider-neutral, paged session history and literal search.
//!
//! ACP's canonical source is the durable `chat_blocks` journal. Terminal
//! providers keep native JSONL outside Loom, so their existing Iris normalizer
//! is read on demand (with `chatlog`'s file-fingerprint cache) and the archived
//! Iris capture remains the durable fallback. This module flattens either source
//! into one honest record vocabulary without manufacturing data a provider did
//! not supply.

use std::collections::HashSet;

use serde_json::Value;
use weaver_api::{HistoryLocationView, HistoryPageView, HistoryRecordView};
use weaver_core::branch::Branch;
use weaver_core::transcript::iris::{Block, Log, Role};

use crate::chat::kind;
use crate::session::Session;
use crate::Db;

pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 200;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const KINDS: &[&str] = &[
    "message",
    "reasoning",
    "tool_call",
    "tool_result",
    "context",
    "event",
    "image",
];

#[derive(Debug, Default)]
pub struct PageOptions {
    pub before: Option<String>,
    pub limit: Option<usize>,
    pub kinds: Vec<String>,
    pub query: Option<String>,
}

#[derive(Debug)]
pub enum PageError {
    BadRequest(String),
    Internal(anyhow::Error),
}

impl PageError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }
}

impl From<anyhow::Error> for PageError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

/// Load the source appropriate to `session`, normalize it, filter it, and return
/// a newest-tail page in chronological display order.
pub async fn page(
    db: &Db,
    session: &Session,
    branch: &Branch,
    options: PageOptions,
) -> Result<HistoryPageView, PageError> {
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(PageError::bad_request(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    let kinds = validate_kinds(&options.kinds)?;
    let query = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    if options.query.is_some() && query.is_none() {
        return Err(PageError::bad_request("q must not be empty"));
    }
    if options
        .query
        .as_ref()
        .is_some_and(|query| query.len() > MAX_QUERY_BYTES)
    {
        return Err(PageError::bad_request(format!(
            "q must be at most {MAX_QUERY_BYTES} bytes"
        )));
    }

    let (source, records) = load(db, session, branch).await?;
    let end = match options.before.as_deref() {
        Some(before) => records
            .iter()
            .position(|record| record.cursor == before)
            .ok_or_else(|| {
                PageError::bad_request("before is not a cursor from this session history")
            })?,
        None => records.len(),
    };
    let mut matching = records[..end]
        .iter()
        .filter(|record| kinds.is_empty() || kinds.contains(record.kind.as_str()))
        .filter(|record| {
            query
                .as_deref()
                .is_none_or(|needle| searchable_text(record).to_lowercase().contains(needle))
        })
        .cloned()
        .collect::<Vec<_>>();
    let has_more = matching.len() > limit;
    if has_more {
        matching.drain(..matching.len() - limit);
    }
    let older_cursor = has_more.then(|| {
        matching
            .first()
            .expect("a non-zero page limit retains a record")
            .cursor
            .clone()
    });
    Ok(HistoryPageView {
        source,
        records: matching,
        older_cursor,
    })
}

fn validate_kinds(kinds: &[String]) -> Result<HashSet<&str>, PageError> {
    let mut out = HashSet::new();
    for value in kinds {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !KINDS.contains(&value) {
            return Err(PageError::bad_request(format!(
                "unknown history kind '{value}'; expected one of {}",
                KINDS.join(", ")
            )));
        }
        out.insert(value);
    }
    Ok(out)
}

async fn load(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> Result<(String, Vec<HistoryRecordView>), anyhow::Error> {
    if session.protocol == "acp" {
        let blocks = crate::chat::list(db, &session.id).await?;
        return Ok((
            "acp".to_string(),
            blocks.iter().map(acp_record).collect::<Vec<_>>(),
        ));
    }
    Ok(
        match crate::chatlog::conversation(db, session, branch).await {
            Some(log) => {
                let source = log.source.clone();
                (source, iris_records(&log))
            }
            None => (session.agent_kind.clone(), Vec::new()),
        },
    )
}

fn acp_record(block: &crate::chat::ChatBlockView) -> HistoryRecordView {
    let payload = &block.payload;
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let mut record = blank(
        format!("acp:{}:{}", block.turn, block.seq),
        "event",
        Some(block.created_at.clone()),
    );
    match block.kind.as_str() {
        kind::USER_MESSAGE => {
            record.kind = "message".to_string();
            record.role = Some("user".to_string());
            record.content = text("text");
        }
        kind::AGENT_MESSAGE => {
            record.kind = "message".to_string();
            record.role = Some("assistant".to_string());
            record.content = text("text");
        }
        kind::THOUGHT => {
            record.kind = "reasoning".to_string();
            record.role = Some("assistant".to_string());
            record.content = text("text");
        }
        kind::TOOL_CALL => {
            record.kind = "tool_call".to_string();
            record.tool_name = text("title").or_else(|| text("tool_kind"));
            // ACP ToolCall has no invocation-arguments field. Do not reinterpret
            // output content or locations as input.
            record.content = Some(crate::chatlog::tool_content_text(payload.get("content")))
                .filter(|text| !text.is_empty());
            record.tool_status = text("status");
            record.is_error = Some(record.tool_status.as_deref() == Some("failed"));
            record.locations = payload
                .get("locations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|location| {
                    Some(HistoryLocationView {
                        path: location.get("path")?.as_str()?.to_string(),
                        line: location.get("line").and_then(Value::as_u64),
                    })
                })
                .collect();
        }
        other => {
            record.event_name = Some(other.to_string());
            record.content = Some(if other == kind::TURN_END {
                let reason = payload
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                format!("turn ended: {reason}")
            } else {
                crate::chatlog::context_note(other, payload)
            });
        }
    }
    record
}

fn iris_records(log: &Log) -> Vec<HistoryRecordView> {
    let mut records = Vec::new();
    for (message_index, message) in log.messages.iter().enumerate() {
        for (block_index, block) in message.blocks.iter().enumerate() {
            let cursor = format!("iris:{message_index}:{block_index}");
            let timestamp = message.timestamp.clone();
            let role = role_name(message.role).to_string();
            let mut record = match block {
                Block::Text { text } => {
                    let kind = if message.role == Role::Context {
                        "context"
                    } else {
                        "message"
                    };
                    let mut record = blank(cursor, kind, timestamp);
                    record.role = Some(role);
                    record.content = nonempty(text);
                    record
                }
                Block::Thinking { text } => {
                    let mut record = blank(cursor, "reasoning", timestamp);
                    record.role = Some(role);
                    record.content = nonempty(text);
                    record
                }
                Block::ToolUse { name, input } => {
                    let mut record = blank(cursor, "tool_call", timestamp);
                    record.role = Some(role);
                    record.tool_name = nonempty(name);
                    record.tool_input = (!input.is_null()).then(|| input.clone());
                    record
                }
                Block::ToolResult { output, is_error } => {
                    let mut record = blank(cursor, "tool_result", timestamp);
                    record.role = Some(role);
                    record.content = nonempty(output);
                    record.is_error = Some(*is_error);
                    record
                }
                Block::Image => blank(cursor, "image", timestamp),
            };
            if record.kind == "context" {
                record.event_name = Some("context".to_string());
            }
            records.push(record);
        }
    }
    records
}

fn blank(cursor: String, kind: &str, timestamp: Option<String>) -> HistoryRecordView {
    HistoryRecordView {
        cursor,
        kind: kind.to_string(),
        role: None,
        content: None,
        tool_name: None,
        tool_input: None,
        tool_status: None,
        is_error: None,
        event_name: None,
        locations: Vec::new(),
        timestamp,
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Context => "context",
    }
}

fn searchable_text(record: &HistoryRecordView) -> String {
    let mut parts = vec![record.kind.as_str()];
    for value in [
        record.role.as_deref(),
        record.content.as_deref(),
        record.tool_name.as_deref(),
        record.tool_status.as_deref(),
        record.event_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        parts.push(value);
    }
    let input = record
        .tool_input
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    let locations = record
        .locations
        .iter()
        .map(|location| location.path.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} {input} {locations}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use weaver_core::transcript::iris::{Message, Role};

    #[test]
    fn iris_tool_input_is_optional_but_preserved_when_present() {
        let log = Log {
            source: "codex".to_string(),
            messages: vec![Message::new(
                Role::Assistant,
                Some("now".to_string()),
                vec![
                    Block::ToolUse {
                        name: "exec".to_string(),
                        input: json!({ "cmd": "cargo test" }),
                    },
                    Block::tool_result("ok", false),
                ],
            )],
            ..Default::default()
        };
        let records = iris_records(&log);
        assert_eq!(records[0].kind, "tool_call");
        assert_eq!(records[0].tool_input, Some(json!({ "cmd": "cargo test" })));
        assert_eq!(records[1].kind, "tool_result");
    }

    #[test]
    fn acp_tool_activity_does_not_invent_arguments() {
        let record = acp_record(&crate::chat::ChatBlockView {
            turn: 2,
            seq: 3,
            kind: kind::TOOL_CALL.to_string(),
            payload: json!({
                "title": "Run tests",
                "status": "completed",
                "content": [{ "type": "text", "text": "all green" }],
                "locations": [{ "path": "/repo/src.rs", "line": 7 }]
            }),
            created_at: "now".to_string(),
        });
        assert_eq!(record.cursor, "acp:2:3");
        assert_eq!(record.tool_name.as_deref(), Some("Run tests"));
        assert_eq!(record.content.as_deref(), Some("all green"));
        assert!(record.tool_input.is_none());
    }
}
