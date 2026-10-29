//! Provider-neutral, paged session history and literal search.
//!
//! ACP's canonical source is the durable `chat_blocks` journal. Terminal
//! providers keep native JSONL outside Loom, so their existing Iris normalizer
//! is read on demand (with `chatlog`'s file-fingerprint cache) and the archived
//! Iris capture remains the durable fallback. This module flattens either source
//! into one record vocabulary without manufacturing data a provider did not
//! supply.

use serde_json::Value;
use weaver_api::{HistoryLocationView, HistoryPageView, HistoryRecordView};
use weaver_core::branch::Branch;
use weaver_core::transcript::iris::{Block, Log, Role};

use crate::chat::kind;
use crate::session::Session;
use crate::Db;

pub const DEFAULT_LIMIT: usize = 100;
pub use weaver_api::operations::sessions::history::{MAX_LIMIT, MAX_QUERY_BYTES};

#[derive(Debug, Default)]
pub struct PageOptions {
    pub offset: usize,
    pub role: Option<weaver_api::HistoryRole>,
    pub limit: Option<usize>,
    pub kinds: Vec<weaver_api::HistoryKind>,
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

/// Immutable normalized history shared by live API reads and handoff retrieval.
/// A handoff captures this once, before replacing the source provider.
#[derive(Clone)]
pub struct Snapshot {
    source: String,
    records: Vec<HistoryRecordView>,
}

impl Snapshot {
    pub fn from_acp(blocks: &[crate::chat::ChatBlockView]) -> Self {
        Self {
            source: "acp".to_string(),
            records: blocks.iter().map(acp_record).collect(),
        }
    }

    pub fn page(&self, options: PageOptions) -> Result<HistoryPageView, PageError> {
        let limit = options.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(PageError::bad_request(format!(
                "limit must be between 1 and {MAX_LIMIT}"
            )));
        }
        let query = options
            .query
            .as_deref()
            .map(str::trim)
            .map(str::to_lowercase);
        if let Some(query) = &options.query {
            if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
                return Err(PageError::bad_request(format!(
                    "q must contain 1 to {MAX_QUERY_BYTES} bytes"
                )));
            }
        }
        let mut matching = self
            .records
            .iter()
            .rev()
            .filter(|record| {
                (options.kinds.is_empty()
                    || options
                        .kinds
                        .iter()
                        .any(|kind| kind.as_str() == record.kind))
                    && options
                        .role
                        .is_none_or(|role| record.role.as_deref() == Some(role.as_str()))
                    && query.as_ref().is_none_or(|needle| {
                        searchable_text(record).to_lowercase().contains(needle)
                    })
            })
            .skip(options.offset);
        let mut records: Vec<_> = matching.by_ref().take(limit).cloned().collect();
        let next_offset = matching
            .next()
            .map(|_| options.offset.saturating_add(records.len()));
        records.reverse();
        Ok(HistoryPageView {
            source: self.source.clone(),
            records,
            next_offset,
        })
    }
}

/// Normalize the session's durable source, then apply the shared history query.
pub async fn page(
    db: &Db,
    session: &Session,
    branch: &Branch,
    options: PageOptions,
) -> Result<HistoryPageView, PageError> {
    let snapshot = if session.protocol == "acp" {
        Snapshot::from_acp(&crate::chat::list(db, &session.id).await?)
    } else {
        let (source, records) = load_terminal(db, session, branch).await?;
        Snapshot { source, records }
    };
    snapshot.page(options)
}

async fn load_terminal(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> Result<(String, Vec<HistoryRecordView>), anyhow::Error> {
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

    /// An ACP session with `blocks` agent messages, the `n`th reading `m<n>`.
    async fn seed_acp_session(db: &Db, blocks: usize) -> (Session, Branch) {
        let branch = weaver_core::branch::upsert(db, "/repo", "weaver/history", "main")
            .await
            .unwrap();
        crate::session::insert(
            db,
            &crate::session::NewSession {
                id: "histsess".to_string(),
                branch_id: branch.id.clone(),
                work_dir: "/w".to_string(),
                term_session: "weaver-histsess".to_string(),
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
        for seq in 0..blocks {
            crate::chat::insert(
                db,
                "histsess",
                0,
                seq as i64,
                kind::AGENT_MESSAGE,
                &json!({ "text": format!("m{seq}") }),
            )
            .await
            .unwrap();
        }
        let session = crate::session::get(db, "histsess").await.unwrap().unwrap();
        (session, branch)
    }

    #[tokio::test]
    async fn acp_history_pages_and_searches_older_records() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let total = 552;
        let (session, branch) = seed_acp_session(&db, total).await;

        let newest = page(
            &db,
            &session,
            &branch,
            PageOptions {
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let content = |page: &HistoryPageView, at: usize| page.records[at].content.clone().unwrap();
        assert_eq!(newest.records.len(), 2);
        assert_eq!(content(&newest, 0), format!("m{}", total - 2));
        assert_eq!(content(&newest, 1), format!("m{}", total - 1));
        assert_eq!(newest.next_offset, Some(2));

        // Search must include the oldest record, beyond the newest page.
        let found = page(
            &db,
            &session,
            &branch,
            PageOptions {
                query: Some("m0".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(found.records.len(), 1);
        assert_eq!(content(&found, 0), "m0");
        assert!(found.next_offset.is_none(), "nothing older matches");

        let older = page(
            &db,
            &session,
            &branch,
            PageOptions {
                offset: newest.next_offset.unwrap(),
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(content(&older, 0), format!("m{}", total - 3));
    }

    #[tokio::test]
    async fn history_filters_roles_before_applying_offsets() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let (session, branch) = seed_acp_session(&db, 3).await;
        crate::chat::insert(
            &db,
            &session.id,
            1,
            0,
            kind::USER_MESSAGE,
            &json!({"text":"old constraint"}),
        )
        .await
        .unwrap();
        crate::chat::insert(
            &db,
            &session.id,
            2,
            0,
            kind::USER_MESSAGE,
            &json!({"text":"new request"}),
        )
        .await
        .unwrap();
        let snapshot = Snapshot::from_acp(&crate::chat::list(&db, &session.id).await.unwrap());
        let options = || PageOptions {
            role: Some(weaver_api::HistoryRole::User),
            limit: Some(1),
            ..Default::default()
        };
        let newest = snapshot.page(options()).unwrap();
        assert_eq!(newest.records[0].content.as_deref(), Some("new request"));
        assert_eq!(newest.next_offset, Some(1));
        let older = page(
            &db,
            &session,
            &branch,
            PageOptions {
                offset: 1,
                ..options()
            },
        )
        .await
        .unwrap();
        assert_eq!(older.records[0].content.as_deref(), Some("old constraint"));
        assert!(older.next_offset.is_none());
        assert!(snapshot
            .page(PageOptions {
                offset: usize::MAX,
                ..options()
            })
            .unwrap()
            .records
            .is_empty());
    }

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
