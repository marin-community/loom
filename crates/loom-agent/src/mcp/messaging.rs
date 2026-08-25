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

use std::sync::OnceLock;

use weaver_api::operations::{branches, sessions};

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_messaging";

/// The tools this server exports, in the order it advertises them.
///
/// `status_update` is the same operation as `loom_session::status_set`, served
/// here under its own name; the handler below is hand-written because reaching
/// it by name translation would defeat `super::runtime_tool_allowed`.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<sessions::status::set::Op>("status_update"),
            export::<branches::slack::reply::Op>("slack_reply"),
        ]
    })
}

// Both sets are named by this adapter: `status_update` shares a grant with
// `loom_session`'s write tool, and `expand_tool_set` resolves a name to the
// first adapter that recognizes it.
const STATUS_SET: &str = "mcp/messaging/status@v1";
const SLACK_SET: &str = "mcp/slack/message@v1";

pub(super) const ADAPTER: Adapter = Adapter {
    name: "messaging",
    server_name: SERVER_NAME,
    description: "Session status and fixed-thread messaging through Loom routing.",
    capability_sets,
    exports,
    superseded: &[("mcp/messaging/status@v1", "loom/sessions/write@v1")],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        exports()
            .iter()
            .map(|export| {
                let (name, description) = match export.tool {
                    "status_update" => (
                        STATUS_SET,
                        "Update the durable Weaver status and its configured mirrors.",
                    ),
                    _ => (
                        SLACK_SET,
                        "Post a message to the Slack thread fixed to this session.",
                    ),
                };
                CapabilitySet {
                    name,
                    group: "messaging",
                    version: "v1",
                    description,
                    tools: Vec::leak(vec![export.tool]),
                }
            })
            .collect()
    })
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
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
    if super::dispatch::lookup(exports(), name).is_none() {
        bail!("unknown messaging tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("messaging tool '{name}' is not allowed by this session");
    }
    let client = super::runtime_client("messaging")?;
    match name {
        "status_update" => update_status(&client, arguments).await,
        "slack_reply" => {
            super::dispatch::call_tool(&client, SERVER_NAME, exports(), "slack_reply", arguments)
                .await
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
        assert_eq!(capability_sets().len(), 2);
        assert!(capability_sets().iter().all(|set| set.group == "messaging"));
        assert_eq!(
            expand_tool_set("mcp/messaging/status@v1").unwrap(),
            vec!["mcp__loom_messaging__status_update"]
        );
        assert_eq!(tools().as_array().unwrap().len(), 2);
    }
}
