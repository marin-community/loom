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

    if session.protocol == "acp" {
        return acp_page(db, session, &options, limit, &kinds, query.as_deref()).await;
    }

    let (source, records) = load_terminal(db, session, branch).await?;
    let end = match options.before.as_deref() {
        Some(before) => records
            .iter()
            .position(|record| record.cursor == before)
            .ok_or_else(unknown_cursor)?,
        None => records.len(),
    };
    let matching = records[..end]
        .iter()
        .filter(|record| matches(record, &kinds, query.as_deref()))
        .cloned()
        .collect::<Vec<_>>();
    Ok(tail_page(source, matching, limit))
}

/// The newest-tail page of an ACP session's journal.
///
/// The journal is read backwards a chunk at a time and stops as soon as the page
/// is provably full, so an ordinary page costs one bounded query no matter how
/// long the session has been running. Only a search that matches little has to
/// walk the whole journal, and even then it holds one chunk at a time.
async fn acp_page(
    db: &Db,
    session: &Session,
    options: &PageOptions,
    limit: usize,
    kinds: &HashSet<&str>,
    query: Option<&str>,
) -> Result<HistoryPageView, PageError> {
    let mut before = match options.before.as_deref() {
        Some(cursor) => {
            let (turn, seq) = parse_acp_cursor(cursor)?;
            if !crate::chat::block_exists(db, &session.id, turn, seq).await? {
                return Err(unknown_cursor());
            }
            Some((turn, seq))
        }
        None => None,
    };
    let chunk = limit.saturating_add(1).max(SCAN_CHUNK);
    let mut matching: Vec<HistoryRecordView> = Vec::new();
    loop {
        let (blocks, has_older) = crate::chat::list_page(db, &session.id, before, chunk).await?;
        let Some(oldest) = blocks.first() else { break };
        before = Some((oldest.turn, oldest.seq));
        let mut older: Vec<HistoryRecordView> = blocks
            .iter()
            .map(acp_record)
            .filter(|record| matches(record, kinds, query))
            .collect();
        older.append(&mut matching);
        matching = older;
        // One record beyond the page is all it takes to know more remain.
        if matching.len() > limit || !has_older {
            break;
        }
    }
    Ok(tail_page("acp".to_string(), matching, limit))
}

/// Blocks read per backwards step of [`acp_page`]. Large enough that an unfiltered
/// page is a single query, small enough that a whole-journal search stays bounded
/// in memory.
const SCAN_CHUNK: usize = 512;

/// Cut the newest `limit` records out of a chronological run of matching records,
/// plus the cursor the caller pages further back with. `matching` holding more
/// than `limit` is what proves older records remain.
fn tail_page(
    source: String,
    mut matching: Vec<HistoryRecordView>,
    limit: usize,
) -> HistoryPageView {
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
    HistoryPageView {
        source,
        records: matching,
        older_cursor,
    }
}

fn matches(record: &HistoryRecordView, kinds: &HashSet<&str>, query: Option<&str>) -> bool {
    (kinds.is_empty() || kinds.contains(record.kind.as_str()))
        && query.is_none_or(|needle| searchable_text(record).to_lowercase().contains(needle))
}

/// The `(turn, seq)` an `acp:<turn>:<seq>` cursor addresses. A cursor this
/// session never issued is a bad request, not an empty page.
fn parse_acp_cursor(cursor: &str) -> Result<(i64, i64), PageError> {
    let (turn, seq) = cursor
        .strip_prefix("acp:")
        .and_then(|rest| rest.split_once(':'))
        .ok_or_else(unknown_cursor)?;
    let turn = turn.parse().map_err(|_| unknown_cursor())?;
    let seq = seq.parse().map_err(|_| unknown_cursor())?;
    Ok((turn, seq))
}

fn unknown_cursor() -> PageError {
    PageError::bad_request("before is not a cursor from this session history")
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

/// A terminal provider's transcript, normalized. Unlike the ACP journal this has
/// no queryable form — it is a file the Iris normalizer reads whole — so paging
/// happens after the load rather than inside it.
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

    /// The journal is read backwards in bounded steps, so a page has to be
    /// assembled correctly whether it falls inside the first step or beyond it.
    #[tokio::test]
    async fn acp_paging_walks_back_past_one_scan_step() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let total = SCAN_CHUNK + 40;
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
        assert_eq!(
            newest.older_cursor.as_deref(),
            Some(format!("acp:0:{}", total - 2).as_str()),
            "the oldest record of the page is where the next one resumes"
        );

        // The only hit sits older than the first backwards step: the scan has to
        // keep going rather than report an empty page.
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
        assert!(found.older_cursor.is_none(), "nothing older matches");

        let older = page(
            &db,
            &session,
            &branch,
            PageOptions {
                before: newest.older_cursor.clone(),
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(content(&older, 0), format!("m{}", total - 3));
    }

    #[tokio::test]
    async fn acp_rejects_a_cursor_this_session_never_issued() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let (session, branch) = seed_acp_session(&db, 3).await;
        for cursor in ["iris:0:0", "acp:nope", "acp:0:99"] {
            let error = page(
                &db,
                &session,
                &branch,
                PageOptions {
                    before: Some(cursor.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("an unknown cursor is a bad request");
            assert!(matches!(error, PageError::BadRequest(_)), "{cursor}");
        }
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
