//! Tolerant serde DTOs for the ACP messages loom consumes, plus the method
//! names it labels durable records with.
//!
//! The `agent-client-protocol` crate owns the connection — transport,
//! request/response correlation, dispatch — and loom's outgoing requests are
//! declared in `super::requests`. What stays here is the *incoming* payload
//! layer: the crate's schema types are strictly typed and drop or reject what
//! an adapter actually sent (harness config options and update variants are
//! not standardized across adapters — see `catalog::selectors`), so the DTOs
//! below parse leniently. Field names and serde conventions are copied
//! verbatim from the crate's `schema` module (v1) and pinned by the tests
//! below against captured `claude-agent-acp` / `codex-acp` messages, so a
//! real adapter frame deserializes here unchanged. Unknown update/content
//! variants degrade rather than fail (`#[serde(other)]`), keeping loom
//! resilient to adapter additions.

use serde::Deserialize;
use serde_json::Value;

/// Method names (verbatim from the ACP schema).
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_STEERING: &str = "_session/steering";
    pub const SESSION_SET_MODE: &str = "session/set_mode";
    pub const SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";
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
    /// Context-window / cumulative cost usage. `used` and `size` are required by
    /// ACP, but remain optional here so one malformed adapter notification does
    /// not make the whole stream undecodable; loom drops incomplete updates.
    UsageUpdate {
        #[serde(default)]
        used: Option<u64>,
        #[serde(default)]
        size: Option<u64>,
        #[serde(default)]
        cost: Option<UsageCost>,
        #[serde(default, rename = "_meta")]
        meta: UsageMeta,
    },
    /// codex-acp's thread lifecycle. Loom normally owns turn boundaries through
    /// the `session/prompt` response; this closes a recovered adapter-owned turn
    /// that has no prompt response id.
    SessionInfoUpdate {
        #[serde(default, rename = "_meta")]
        meta: SessionInfoMeta,
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
    /// Anything else (prompt_suggestion, …): ignored.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageCost {
    pub amount: f64,
    pub currency: String,
}

/// Adapter provenance attached to a usage update. Claude's ACP adapter uses a
/// cost-bearing `task-notification` update as the only terminal boundary for an
/// autonomous continuation triggered by a completed background task; unlike a
/// human prompt, that continuation has no `session/prompt` response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageMeta {
    #[serde(default, rename = "_claude/origin")]
    pub claude_origin: Option<UsageOrigin>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageOrigin {
    pub kind: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionInfoMeta {
    #[serde(default)]
    pub codex: Option<CodexSessionInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionInfo {
    #[serde(default)]
    pub thread_status: Option<ThreadStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadStatus {
    #[serde(rename = "type")]
    pub kind: String,
}

/// A displayable ACP content block.
///
/// Agent prose consumes only [`ContentBlock::Text`], while tool calls also
/// preserve images for the durable chat journal and Conversation preview.
/// Other protocol content (audio and future variants) degrades to
/// [`ContentBlock::Other`] until loom has a truthful renderer for it.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default)]
        uri: Option<String>,
    },
    ResourceLink {
        uri: String,
        #[serde(default, rename = "mimeType")]
        mime_type: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl ContentBlock {
    /// The text this block carries, or `None` for a non-text block.
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            ContentBlock::Image { .. }
            | ContentBlock::ResourceLink { .. }
            | ContentBlock::Other => None,
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
    pub name: Option<String>,
    #[serde(default)]
    pub raw_input: Option<Value>,
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

/// The `initialize` result capabilities loom uses, plus adapter extensions.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default, rename = "_meta")]
    pub meta: InitializeMeta,
}

/// The subset of agent capabilities loom checks.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    #[serde(default, rename = "_meta")]
    pub meta: AgentCapabilitiesMeta,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InitializeMeta {
    #[serde(default)]
    pub steering: SteeringCapability,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentCapabilitiesMeta {
    #[serde(default, rename = "claudeCode")]
    pub claude_code: ClaudeCodeCapabilities,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeCapabilities {
    #[serde(default)]
    pub prompt_queueing: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SteeringCapability {
    #[serde(default)]
    pub supported: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum SteeringResult {
    Injected,
    PromptRequired,
    StartedNewTurn,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            "content": { "type": "audio", "data": "…", "mimeType": "audio/wav" },
        }))
        .unwrap();
        match u {
            SessionUpdate::AgentThoughtChunk { content, .. } => assert_eq!(content.text(), None),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_tool_call_with_image_content() {
        let u: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_image",
            "title": "Read screenshot.png",
            "kind": "read",
            "status": "completed",
            "content": [{
                "type": "content",
                "content": {
                    "type": "image",
                    "data": "aW1hZ2U=",
                    "mimeType": "image/png",
                    "uri": "file:///tmp/screenshot.png"
                }
            }]
        }))
        .unwrap();

        match u {
            SessionUpdate::ToolCall(tc) => match &tc.content.unwrap()[0] {
                ToolCallContent::Content {
                    content:
                        ContentBlock::Image {
                            data,
                            mime_type,
                            uri,
                        },
                } => {
                    assert_eq!(data, "aW1hZ2U=");
                    assert_eq!(mime_type, "image/png");
                    assert_eq!(uri.as_deref(), Some("file:///tmp/screenshot.png"));
                }
                other => panic!("wrong image content: {other:?}"),
            },
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
            "cost": { "amount": 1.25, "currency": "USD" },
        }))
        .unwrap();
        match usage {
            SessionUpdate::UsageUpdate {
                used,
                size,
                cost,
                meta,
            } => {
                assert_eq!(used, Some(41000));
                assert_eq!(size, Some(200000));
                let cost = cost.unwrap();
                assert_eq!(cost.amount, 1.25);
                assert_eq!(cost.currency, "USD");
                assert!(meta.claude_origin.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let task_usage: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "usage_update",
            "used": 42000,
            "size": 200000,
            "cost": { "amount": 1.5, "currency": "USD" },
            "_meta": { "_claude/origin": { "kind": "task-notification" } },
        }))
        .unwrap();
        match task_usage {
            SessionUpdate::UsageUpdate { meta, .. } => {
                assert_eq!(meta.claude_origin.unwrap().kind, "task-notification")
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

        let info: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "session_info_update",
            "_meta": { "codex": { "threadStatus": { "type": "idle" } } },
        }))
        .unwrap();
        match info {
            SessionUpdate::SessionInfoUpdate { meta } => {
                assert_eq!(meta.codex.unwrap().thread_status.unwrap().kind, "idle");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // An update kind loom does not model must not fail the stream.
        let other: SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "prompt_suggestion", "title": "hello",
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
        assert_eq!(p.options[0].kind, "allow_once");
        assert_eq!(p.options[1].kind, "reject_once");
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

        let init: InitializeResult = serde_json::from_value(json!({
            "agentCapabilities": {
                "loadSession": true,
                "_meta": { "claudeCode": { "promptQueueing": true } },
            },
            "_meta": { "steering": { "supported": true } },
        }))
        .unwrap();
        assert!(init.agent_capabilities.load_session);
        assert!(init.agent_capabilities.meta.claude_code.prompt_queueing);
        assert!(init.meta.steering.supported);
    }
}
