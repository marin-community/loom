//! Built-in session messaging MCP adapter.
//!
//! `slack_reply` is the registered `branches.slack.reply` operation
//! (`mcp = "loom_messaging::slack_reply"`) and routes straight through
//! `super::dispatch::call_tool`.
//!
//! `status_update` is hand-written because the allow-list check
//! (`super::runtime_tool_allowed`) is keyed by the tool name it receives against
//! `LOOM_MCP_ALLOWED_TOOLS`. Aliasing to `loom_session::status_set` would check
//! for a name the allow-list never contains, causing a scoped session to reject
//! the call. Both schemas are hand-written: `status_update` has no registered
//! `loom_messaging::*` projection, and `slack_reply`'s `thread` shape and wording
//! preserve what sessions pinned to `mcp/messaging/status@v1`/`mcp/slack/message@v1` see.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_messaging";
const TOOL_NAMES: [&str; 2] = ["status_update", "slack_reply"];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "messaging",
    server_name: SERVER_NAME,
    description: "Session status and fixed-thread messaging through Loom routing.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

const STATUS_TOOLS: &[&str] = &["status_update"];
const SLACK_TOOLS: &[&str] = &["slack_reply"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "mcp/messaging/status@v1",
        group: "messaging",
        version: "v1",
        description: "Update the durable Weaver status and its configured mirrors.",
        tools: STATUS_TOOLS,
    },
    CapabilitySet {
        name: "mcp/slack/message@v1",
        group: "messaging",
        version: "v1",
        description: "Post a message to the Slack thread fixed to this session.",
        tools: SLACK_TOOLS,
    },
];

fn capability_sets() -> &'static [CapabilitySet] {
    CAPABILITY_SETS
}

fn is_permission_rule(rule: &str) -> bool {
    super::is_builtin_permission_rule(SERVER_NAME, &TOOL_NAMES, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::expand_builtin_tool_set(SERVER_NAME, &TOOL_NAMES, CAPABILITY_SETS, name)
}

fn server_config() -> Value {
    super::builtin_server_config("messaging")
}

fn tools() -> Value {
    json!([
        {
            "name": "status_update",
            "description": "Update this session's durable status. Configured GitHub and Slack status cards are updated automatically.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "level": { "type": "string", "enum": ["ok", "attention", "blocked"] },
                    "message": { "type": "string", "maxLength": 4096 }
                },
                "required": ["level", "message"]
            }
        },
        {
            "name": "slack_reply",
            "description": "Post a message to a Slack thread this session owns. Omit 'thread' for the thread fixed to this session. Pass 'thread' to answer in a thread an automation delivery announced to this session — an alert's own thread, whose channel and thread_ts arrive with the alert.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "text": { "type": "string", "minLength": 1, "maxLength": 4000 },
                    "idempotency_key": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": weaver_api::CHANNEL_IDEMPOTENCY_KEY_MAX_LEN,
                        "description": "For the session's origin thread, retry safely with the same key."
                    },
                    "thread": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "channel": { "type": "string" },
                            "thread_ts": { "type": "string" }
                        },
                        "required": ["channel", "thread_ts"]
                    }
                },
                "required": ["text"]
            }
        }
    ])
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown messaging tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("messaging tool '{name}' is not allowed by this session");
    }
    let client = super::runtime_client("messaging")?;
    match name {
        "status_update" => update_status(&client, arguments).await,
        "slack_reply" => {
            super::dispatch::call_tool(&client, SERVER_NAME, "slack_reply", arguments).await
        }
        _ => unreachable!(),
    }
}

/// `sessions.status.set` (`loom_session::status_set`) is the same operation
/// this tool exposes, but reached by name translation that would defeat
/// `super::runtime_tool_allowed`'s allow-list check — see the module doc
/// comment — so this calls the underlying REST route directly instead of
/// `super::dispatch::call_tool`.
async fn update_status(client: &weaver_api::Client, arguments: Value) -> Result<Value> {
    let level = arguments
        .get("level")
        .and_then(Value::as_str)
        .context("status_update requires level")?;
    let message = arguments
        .get("message")
        .and_then(Value::as_str)
        .context("status_update requires message")?;
    if message.len() > 4096 {
        bail!("status_update message must be at most 4096 bytes");
    }
    let session_id =
        std::env::var("LOOM_SESSION_ID").context("messaging MCP is missing LOOM_SESSION_ID")?;
    let session = client
        .get_session(&session_id)
        .await
        .context("resolving the messaging MCP session")?;
    client
        .set_branch_status(&session.branch.id, level, message)
        .await?;
    let text = format!("status updated to {level}");
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "message": text },
        "isError": false
    }))
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messaging_sets_are_grouped_and_exact() {
        assert_eq!(CAPABILITY_SETS.len(), 2);
        assert!(CAPABILITY_SETS.iter().all(|set| set.group == "messaging"));
        assert_eq!(
            expand_tool_set("mcp/messaging/status@v1").unwrap(),
            vec!["mcp__loom_messaging__status_update"]
        );
        assert_eq!(tools().as_array().unwrap().len(), 2);
    }

    /// `slack_reply` is a real `loom_messaging` projection; `status_update`
    /// is not (it rides on `loom_session::status_set` instead), so only one
    /// of the two tool names this adapter advertises resolves in the
    /// registry under its own server.
    #[test]
    fn slack_reply_resolves_in_the_registry_status_update_does_not() {
        assert!(weaver_api::operation_for_mcp(SERVER_NAME, "slack_reply").is_some());
        assert!(weaver_api::operation_for_mcp(SERVER_NAME, "status_update").is_none());
        assert!(weaver_api::operation_for_mcp("loom_session", "status_set").is_some());
    }
}
