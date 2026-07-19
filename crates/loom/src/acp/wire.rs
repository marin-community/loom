//! The ACP wire format loom speaks over the relay: a thin JSON-RPC 2.0 envelope
//! plus minimal serde structs for the Agent Client Protocol messages loom
//! consumes and produces.
//!
//! These are hand-rolled rather than pulled from the `agent-client-protocol`
//! crate: that crate is a full builder/runtime SDK (its own transport,
//! `Client.builder()`, session runners) and its schema types are `#[non_exhaustive]`
//! builder structs that fight direct serde use. The field names and serde
//! conventions here are copied verbatim from that crate's `schema` module (v1),
//! and pinned by the serialization tests below against the exact wire shapes the
//! real adapters emit — so a captured `claude-agent-acp` / `codex-acp` message
//! deserializes here unchanged. Unknown update/content variants degrade rather
//! than fail (`#[serde(other)]`), keeping loom resilient to adapter additions.

use serde::Deserialize;
use serde_json::Value;

/// A parsed inbound JSON-RPC message (one relay frame = one newline-delimited
/// JSON object). Classified by which fields are present:
/// - `method` + `id`  → an agent→client **request** (only `session/request_permission`).
/// - `method`, no `id` → an agent→client **notification** (`session/update`, …).
/// - `id`, no `method` → a **response** to one of loom's requests.
#[derive(Debug, Clone, Deserialize)]
pub struct Incoming {
    /// Request/response id (number or string). Echoed verbatim when we answer an
    /// agent request, so it is carried as a raw [`Value`].
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
}

impl Incoming {
    /// Classify this message.
    pub fn kind(&self) -> IncomingKind {
        match (&self.method, &self.id) {
            (Some(_), Some(_)) => IncomingKind::Request,
            (Some(_), None) => IncomingKind::Notification,
            (None, Some(_)) => IncomingKind::Response,
            (None, None) => IncomingKind::Unknown,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IncomingKind {
    Request,
    Notification,
    Response,
    Unknown,
}

/// Method names (verbatim from the ACP schema).
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_SET_MODE: &str = "session/set_mode";
    pub const SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";
    pub const SESSION_UPDATE: &str = "session/update";
    pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
}

/// Serialize a client→agent request line (`{jsonrpc, id, method, params}` + `\n`).
pub fn request_line(id: u64, method: &str, params: Value) -> Vec<u8> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    line(&msg)
}

/// Serialize a client→agent notification line (no id).
pub fn notification_line(method: &str, params: Value) -> Vec<u8> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    line(&msg)
}

/// Serialize a client→agent response line answering an agent request `id` (echoed
/// verbatim) with `result`.
pub fn response_line(id: &Value, result: Value) -> Vec<u8> {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    line(&msg)
}

fn line(v: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(v).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

// ---------------------------------------------------------------------------
// session/update notification
// ---------------------------------------------------------------------------

/// The `session/update` notification params.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    #[allow(dead_code)]
    pub session_id: String,
    pub update: SessionUpdate,
}

/// A `session/update` variant, discriminated by the `sessionUpdate` field. Only
/// the variants loom journals are modeled; anything else degrades to
/// [`SessionUpdate::Other`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    /// A chunk of the user's message — the adapter echoing or replaying a user
    /// turn (`session/load`, post-compact context replay). Recognized so it never
    /// falls into [`SessionUpdate::Other`], but its payload is deliberately
    /// dropped: loom journals every prompt itself at dispatch, so user chunks
    /// carry nothing new (see `crate::acp`).
    UserMessageChunk,
    /// A chunk of the agent's streamed reply.
    AgentMessageChunk { content: ContentBlock },
    /// A chunk of the agent's streamed reasoning.
    AgentThoughtChunk { content: ContentBlock },
    /// A new tool call was initiated.
    ToolCall(ToolCall),
    /// A status/content update on an existing tool call (same flat shape).
    ToolCallUpdate(ToolCall),
    /// The agent's plan (a full checklist; replaces the prior one).
    Plan(Plan),
    /// The current session mode changed.
    CurrentModeUpdate {
        #[serde(rename = "currentModeId")]
        current_mode_id: String,
    },
    /// Context-window / cost usage.
    UsageUpdate {
        #[serde(default)]
        used: Option<u64>,
        #[serde(default)]
        size: Option<u64>,
    },
    /// The agent-owned slash-command catalogue. Clients replace their cached
    /// list wholesale on every update.
    AvailableCommandsUpdate {
        #[serde(default, rename = "availableCommands")]
        available_commands: Vec<Value>,
    },
    /// The full set of live session configuration controls (model, reasoning
    /// effort, mode, and adapter-specific options).
    ConfigOptionUpdate {
        #[serde(default, rename = "configOptions")]
        config_options: Vec<Value>,
    },
    /// Anything else (session_info_update, prompt_suggestion, …): ignored.
    #[serde(other)]
    Other,
}

/// A content block. Only text is consumed; other types degrade to
/// [`ContentBlock::Other`] (mapped to empty text by [`ContentBlock::text`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

impl ContentBlock {
    /// The text this block carries, or `None` for a non-text block.
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            ContentBlock::Other => None,
        }
    }
}

/// A tool call (or update — the wire shape is identical, all fields but the id
/// optional on an update).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub content: Option<Vec<ToolCallContent>>,
    #[serde(default)]
    pub locations: Option<Vec<ToolCallLocation>>,
}

/// Content produced by a tool call.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallContent {
    /// A standard content block (text/image/…).
    Content { content: ContentBlock },
    /// A file diff.
    Diff {
        path: String,
        #[serde(default, rename = "oldText")]
        old_text: Option<String>,
        #[serde(rename = "newText")]
        new_text: String,
    },
    /// Terminal embeds and any future type degrade to nothing.
    #[serde(other)]
    Other,
}

/// A file location a tool call touched (for follow-along).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
}

/// The agent's plan.
#[derive(Debug, Clone, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub entries: Vec<PlanEntry>,
}

/// One plan entry.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// session/request_permission request
// ---------------------------------------------------------------------------

/// The `session/request_permission` request params.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionParams {
    #[allow(dead_code)]
    pub session_id: String,
    /// Details of the gated tool call (the same flat tool-call shape).
    pub tool_call: ToolCall,
    pub options: Vec<PermissionOption>,
}

/// One permission option offered to the client.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

impl PermissionOption {
    /// Whether this option grants the action (an `allow_*` kind) — used to pick a
    /// default when auto-answering.
    pub fn is_allow(&self) -> bool {
        self.kind.starts_with("allow")
    }
}

// ---------------------------------------------------------------------------
// Response bodies loom parses
// ---------------------------------------------------------------------------

/// The `session/new` result.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub config_options: Option<Vec<Value>>,
}

/// The `session/load` result (mode state only; history arrives as `session/update`
/// notifications during the call).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionResult {
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub config_options: Option<Vec<Value>>,
}

/// The active mode + available modes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<Value>,
}

/// The `session/set_config_option` result. ACP returns the complete refreshed
/// option set so the client never has to guess coupled changes (for example a
/// model switch changing the available reasoning levels).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigOptionResult {
    #[serde(default)]
    pub config_options: Vec<Value>,
}

/// The `session/prompt` result — the turn's stop reason.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub stop_reason: String,
}

/// The `initialize` result — the agent's capabilities (we read `loadSession`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
}

/// The subset of agent capabilities loom checks.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
}

// ---------------------------------------------------------------------------
// Request params builders (client → agent)
// ---------------------------------------------------------------------------

/// The ACP protocol version loom speaks (serialized as a bare integer).
pub const PROTOCOL_VERSION: u16 = 1;

/// `initialize` params.
pub fn initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {},
    })
}

/// `session/new` params. `meta` is the optional `_meta` object (adapter options
/// such as `{"claudeCode":{"options":{...}}}`).
pub fn new_session_params(cwd: &str, meta: Option<&Value>) -> Value {
    let mut v = serde_json::json!({ "cwd": cwd, "mcpServers": [] });
    if let Some(meta) = meta {
        v["_meta"] = meta.clone();
    }
    v
}

/// `session/load` params.
pub fn load_session_params(session_id: &str, cwd: &str) -> Value {
    serde_json::json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": [] })
}

/// `session/prompt` params carrying a single text block.
pub fn prompt_params(session_id: &str, text: &str) -> Value {
    serde_json::json!({
        "sessionId": session_id,
        "prompt": [ { "type": "text", "text": text } ],
    })
}

/// `session/cancel` notification params.
pub fn cancel_params(session_id: &str) -> Value {
    serde_json::json!({ "sessionId": session_id })
}

/// `session/set_mode` params.
pub fn set_mode_params(session_id: &str, mode_id: &str) -> Value {
    serde_json::json!({ "sessionId": session_id, "modeId": mode_id })
}

/// `session/set_config_option` params. ACP configuration values are typed: most
/// composer controls are select strings, while flags such as Codex fast mode are
/// booleans.
pub fn set_config_option_params(session_id: &str, config_id: &str, value: Value) -> Value {
    serde_json::json!({ "sessionId": session_id, "configId": config_id, "value": value })
}

/// A `session/request_permission` response selecting an option.
pub fn permission_selected(option_id: &str) -> Value {
    serde_json::json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

/// A `session/request_permission` response reporting the turn was cancelled.
pub fn permission_cancelled() -> Value {
    serde_json::json!({ "outcome": { "outcome": "cancelled" } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_inbound_messages() {
        let notif: Incoming = serde_json::from_value(json!({
            "jsonrpc":"2.0","method":"session/update","params":{}
        }))
        .unwrap();
        assert_eq!(notif.kind(), IncomingKind::Notification);

        let req: Incoming = serde_json::from_value(json!({
            "jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{}
        }))
        .unwrap();
        assert_eq!(req.kind(), IncomingKind::Request);
        assert_eq!(req.id, Some(json!(7)));

        let resp: Incoming = serde_json::from_value(json!({
            "jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}
        }))
        .unwrap();
        assert_eq!(resp.kind(), IncomingKind::Response);
    }

    #[test]
    fn deserializes_agent_message_chunk() {
        // The exact shape claude-agent-acp streams.
        let u: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello" },
        }))
        .unwrap();
        match u {
            SessionUpdate::AgentMessageChunk { content, .. } => {
                assert_eq!(content.text(), Some("hello"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_thought_chunk_and_unknown_content_degrades() {
        let u: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "image", "data": "…", "mimeType": "image/png" },
        }))
        .unwrap();
        match u {
            SessionUpdate::AgentThoughtChunk { content, .. } => assert_eq!(content.text(), None),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_tool_call_with_diff_and_locations() {
        let u: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "Edit web.rs",
            "kind": "edit",
            "status": "completed",
            "content": [
                { "type": "diff", "path": "/w/web.rs", "oldText": "a", "newText": "b" },
                { "type": "content", "content": { "type": "text", "text": "done" } }
            ],
            "locations": [ { "path": "/w/web.rs", "line": 12 } ],
        }))
        .unwrap();
        match u {
            SessionUpdate::ToolCall(tc) => {
                assert_eq!(tc.tool_call_id, "call_1");
                assert_eq!(tc.kind.as_deref(), Some("edit"));
                assert_eq!(tc.status.as_deref(), Some("completed"));
                let content = tc.content.unwrap();
                match &content[0] {
                    ToolCallContent::Diff {
                        path,
                        old_text,
                        new_text,
                    } => {
                        assert_eq!(path, "/w/web.rs");
                        assert_eq!(old_text.as_deref(), Some("a"));
                        assert_eq!(new_text, "b");
                    }
                    other => panic!("wrong content 0: {other:?}"),
                }
                match &content[1] {
                    ToolCallContent::Content { content } => {
                        assert_eq!(content.text(), Some("done"))
                    }
                    other => panic!("wrong content 1: {other:?}"),
                }
                let loc = tc.locations.unwrap();
                assert_eq!(loc[0].path, "/w/web.rs");
                assert_eq!(loc[0].line, Some(12));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn tool_call_update_uses_the_same_flat_shape() {
        let u: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "failed",
        }))
        .unwrap();
        match u {
            SessionUpdate::ToolCallUpdate(tc) => {
                assert_eq!(tc.tool_call_id, "call_1");
                assert_eq!(tc.status.as_deref(), Some("failed"));
                assert!(tc.title.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_plan_usage_mode_and_unknown_update() {
        let plan: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "plan",
            "entries": [ { "content": "trace url", "priority": "high", "status": "completed" } ],
        }))
        .unwrap();
        match plan {
            SessionUpdate::Plan(p) => {
                assert_eq!(p.entries[0].content, "trace url");
                assert_eq!(p.entries[0].status, "completed");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let usage: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "usage_update", "used": 41000, "size": 200000,
        }))
        .unwrap();
        match usage {
            SessionUpdate::UsageUpdate { used, size } => {
                assert_eq!(used, Some(41000));
                assert_eq!(size, Some(200000));
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let mode: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "current_mode_update", "currentModeId": "acceptEdits",
        }))
        .unwrap();
        match mode {
            SessionUpdate::CurrentModeUpdate { current_mode_id } => {
                assert_eq!(current_mode_id, "acceptEdits");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let commands: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": [{"name":"review","description":"Review changes"}],
        }))
        .unwrap();
        match commands {
            SessionUpdate::AvailableCommandsUpdate { available_commands } => {
                assert_eq!(available_commands[0]["name"], "review");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let config: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "config_option_update",
            "configOptions": [{"id":"model","name":"Model","type":"select"}],
        }))
        .unwrap();
        match config {
            SessionUpdate::ConfigOptionUpdate { config_options } => {
                assert_eq!(config_options[0]["id"], "model");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // An update kind loom does not model must not fail the stream.
        let other: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "session_info_update", "title": "hello",
        }))
        .unwrap();
        assert!(matches!(other, SessionUpdate::Other));
    }

    #[test]
    fn deserializes_permission_request() {
        let p: RequestPermissionParams = serde_json::from_value(json!({
            "sessionId": "sess-1",
            "toolCall": { "toolCallId": "call_9", "title": "edit deploy" },
            "options": [
                { "optionId": "allow-once", "name": "Allow once", "kind": "allow_once" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
            ],
        }))
        .unwrap();
        assert_eq!(p.tool_call.tool_call_id, "call_9");
        assert_eq!(p.options.len(), 2);
        assert!(p.options[0].is_allow());
        assert!(!p.options[1].is_allow());
    }

    #[test]
    fn deserializes_new_session_and_prompt_results() {
        let ns: NewSessionResult = serde_json::from_value(json!({
            "sessionId": "acp-abc",
            "modes": { "currentModeId": "default", "availableModes": [] },
            "configOptions": [{"id":"model","name":"Model","type":"select"}],
        }))
        .unwrap();
        assert_eq!(ns.session_id, "acp-abc");
        assert_eq!(ns.modes.unwrap().current_mode_id, "default");
        assert_eq!(ns.config_options.unwrap()[0]["id"], "model");

        let pr: PromptResult = serde_json::from_value(json!({ "stopReason": "end_turn" })).unwrap();
        assert_eq!(pr.stop_reason, "end_turn");
    }

    #[test]
    fn builds_request_and_response_lines() {
        let line = request_line(1, method::INITIALIZE, initialize_params());
        assert!(line.ends_with(b"\n"));
        let v: Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        assert_eq!(v["method"], "initialize");
        assert_eq!(v["id"], 1);
        assert_eq!(v["params"]["protocolVersion"], 1);

        let resp = response_line(&json!(7), permission_selected("allow-once"));
        let v: Value = serde_json::from_slice(&resp[..resp.len() - 1]).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["result"]["outcome"]["outcome"], "selected");
        assert_eq!(v["result"]["outcome"]["optionId"], "allow-once");
    }
}
