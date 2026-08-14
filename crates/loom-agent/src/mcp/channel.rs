//! Durable channel operations projected from Loom's REST API.

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use weaver_api::{CreateChannelMessageReq, CreateChannelReq};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_channel";
const TOOL_NAMES: [&str; 8] = [
    "list",
    "get",
    "read",
    "send",
    "wait",
    "ack",
    "open",
    "subscribe",
];
const READ_TOOLS: &[&str] = &["list", "get", "read", "wait"];
const WRITE_TOOLS: &[&str] = &["send", "ack", "open", "subscribe"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "mcp/channel/read@v1",
        group: "channel",
        version: "v1",
        description: "List, inspect, read, and wait on visible durable channels.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "mcp/channel/write@v1",
        group: "channel",
        version: "v1",
        description: "Send, acknowledge, open, and subscribe to durable channels.",
        tools: WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "channel",
    server_name: SERVER_NAME,
    description: "Durable conversation streams, subscriptions, and delivery receipts.",
    capability_sets: || CAPABILITY_SETS,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn permission_rule(tool: &str) -> Option<String> {
    TOOL_NAMES
        .contains(&tool)
        .then(|| format!("mcp__{SERVER_NAME}__{tool}"))
}

fn is_permission_rule(rule: &str) -> bool {
    TOOL_NAMES
        .iter()
        .any(|tool| permission_rule(tool).as_deref() == Some(rule))
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    CAPABILITY_SETS
        .iter()
        .find(|set| set.name == name)
        .map(|set| {
            set.tools
                .iter()
                .map(|tool| permission_rule(tool).expect("registered channel tool"))
                .collect()
        })
}

fn server_config() -> Value {
    json!({ "type": "stdio", "command": "loom", "args": ["mcp", "serve", "channel"] })
}

fn channel_property() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "A visible channel id. Omit or pass 'self' for this session's channel."
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "list",
            "description": "List channels visible to this session, including unread state and binding summaries.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "archived": { "type": "boolean", "default": false } }
            }
        },
        {
            "name": "get",
            "description": "Get channel metadata and its server-owned delivery bindings.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "channel": channel_property() }
            }
        },
        {
            "name": "read",
            "description": "Read an ordered channel stream without changing its read marker.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "channel": channel_property(),
                    "after": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 },
                    "kinds": { "type": "array", "uniqueItems": true, "items": {
                        "type": "string", "enum": ["goal", "message", "status", "result", "system"]
                    }}
                }
            }
        },
        {
            "name": "send",
            "description": "Append one durable message and return its per-binding delivery receipts. Retrying with the same idempotency_key reuses the item and does not repeat a successful delivery.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "channel": channel_property(),
                    "body": { "type": "string", "minLength": 1, "maxLength": 262144 },
                    "kind": { "type": "string", "enum": ["message", "status", "result"], "default": "message" },
                    "urgency": { "type": "string", "enum": ["normal", "attention", "blocked"], "default": "normal" },
                    "payload": {},
                    "reply_to": { "type": "string", "minLength": 1 },
                    "idempotency_key": { "type": "string", "minLength": 1, "maxLength": 255 }
                },
                "required": ["body"]
            }
        },
        {
            "name": "wait",
            "description": "Wait for the first matching channel item and return it with the new cursor.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "channel": channel_property(),
                    "after": { "type": "integer", "minimum": 0 },
                    "kind": { "type": "string", "enum": ["goal", "message", "status", "result", "system"] },
                    "urgent": { "type": "boolean", "default": false },
                    "timeout": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 1800 }
                }
            }
        },
        {
            "name": "ack",
            "description": "Advance this session's read marker through a sequence, or through the latest item when seq is omitted.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "channel": channel_property(),
                    "seq": { "type": "integer", "minimum": 0 }
                }
            }
        },
        {
            "name": "open",
            "description": "Open a durable custom channel in this repository.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "minLength": 1, "maxLength": 120 },
                    "topic": { "type": "string", "maxLength": 4096, "default": "" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "subscribe",
            "description": "Set observe or runtime-deliver mode for this session or a visible descendant session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "channel": channel_property(),
                    "mode": { "type": "string", "enum": ["observe", "deliver"], "default": "observe" },
                    "session": { "type": "string", "minLength": 1 }
                }
            }
        }
    ])
}

fn object(arguments: &Value) -> Result<&Map<String, Value>> {
    arguments
        .as_object()
        .context("channel tool arguments must be an object")
}

fn string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        Some(value) => Ok(Some(
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .with_context(|| format!("{key} must be a non-empty string"))?,
        )),
        None => Ok(None),
    }
}

async fn resolve_channel(client: &weaver_api::Client, arguments: &Value) -> Result<String> {
    match string(arguments, "channel")? {
        Some(channel) if channel != "self" => Ok(channel.to_string()),
        _ => Ok(client.self_context().await?.channel_id),
    }
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown channel tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("channel tool '{name}' is not allowed by this session");
    }
    object(&arguments)?;
    let client = super::runtime_client("channel")?;
    match name {
        "list" => {
            let archived = arguments
                .get("archived")
                .map(|value| value.as_bool().context("archived must be a boolean"))
                .transpose()?
                .unwrap_or(false);
            let mut items = Vec::new();
            for channel in client.list_channels(archived).await? {
                let bindings = client.channel_bindings(&channel.id).await?;
                items.push(json!({ "channel": channel, "bindings": bindings }));
            }
            super::structured_result(&format!("{} visible channel(s)", items.len()), &items)
        }
        "get" => {
            let id = resolve_channel(&client, &arguments).await?;
            let channel = client.get_channel(&id).await?;
            let bindings = client.channel_bindings(&id).await?;
            let value = json!({ "channel": channel, "bindings": bindings });
            super::structured_result(&format!("channel {id}"), &value)
        }
        "read" => {
            let id = resolve_channel(&client, &arguments).await?;
            let after = arguments.get("after").and_then(Value::as_i64).unwrap_or(0);
            if after < 0 {
                bail!("after must be non-negative");
            }
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100);
            if !(1..=500).contains(&limit) {
                bail!("limit must be between 1 and 500");
            }
            let kinds = arguments
                .get("kinds")
                .map(|value| {
                    value
                        .as_array()
                        .context("kinds must be an array")?
                        .iter()
                        .map(|kind| kind.as_str().context("every kind must be a string"))
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            let fetched = client.channel_messages_bounded(&id, after, 500).await?;
            let scanned_cursor = fetched.last().map(|message| message.seq).unwrap_or(after);
            let messages = fetched
                .into_iter()
                .filter(|message| kinds.is_empty() || kinds.contains(&message.kind.as_str()))
                .take(limit as usize)
                .collect::<Vec<_>>();
            let cursor = messages
                .last()
                .map(|message| message.seq)
                .unwrap_or(scanned_cursor);
            let value = json!({ "channel_id": id, "items": messages, "cursor": cursor });
            super::structured_result(&format!("{} channel item(s)", messages.len()), &value)
        }
        "send" => {
            let id = resolve_channel(&client, &arguments).await?;
            let body = string(&arguments, "body")?.context("send requires body")?;
            let request = CreateChannelMessageReq {
                kind: string(&arguments, "kind")?.unwrap_or("message").to_string(),
                urgency: string(&arguments, "urgency")?
                    .unwrap_or("normal")
                    .to_string(),
                body: body.to_string(),
                payload: arguments
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                reply_to: string(&arguments, "reply_to")?.map(str::to_string),
                idempotency_key: string(&arguments, "idempotency_key")?.map(str::to_string),
            };
            let message = client.send_channel_message(&id, &request).await?;
            super::structured_result(
                &format!(
                    "sent channel item {} with {} delivery receipt(s)",
                    message.id,
                    message.deliveries.len()
                ),
                &message,
            )
        }
        "wait" => {
            let id = resolve_channel(&client, &arguments).await?;
            let channel = client.get_channel(&id).await?;
            let mut cursor = arguments
                .get("after")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| {
                    channel
                        .last_message
                        .as_ref()
                        .map(|message| message.seq)
                        .unwrap_or(0)
                });
            if cursor < 0 {
                bail!("after must be non-negative");
            }
            let kind = string(&arguments, "kind")?;
            let urgent = arguments
                .get("urgent")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let timeout = arguments
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(1800);
            if !(1..=3600).contains(&timeout) {
                bail!("timeout must be between 1 and 3600 seconds");
            }
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
            loop {
                let messages = client.channel_messages_bounded(&id, cursor, 500).await?;
                if let Some(last) = messages.last() {
                    cursor = last.seq;
                }
                if let Some(message) = messages.into_iter().find(|message| {
                    kind.is_none_or(|kind| message.kind == kind)
                        && (!urgent || matches!(message.urgency.as_str(), "attention" | "blocked"))
                }) {
                    let message_cursor = message.seq;
                    let value = json!({ "message": message, "cursor": message_cursor });
                    return super::structured_result("received matching channel item", &value);
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("timed out waiting for channel {id}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        "ack" => {
            let id = resolve_channel(&client, &arguments).await?;
            let seq = arguments
                .get("seq")
                .map(|value| {
                    let seq = value.as_i64().context("seq must be an integer")?;
                    (seq >= 0)
                        .then_some(seq)
                        .context("seq must be non-negative")
                })
                .transpose()?;
            let value = client.mark_channel_read(&id, seq).await?;
            super::structured_result(
                &format!("acknowledged channel {id} through {}", value.read_seq),
                &value,
            )
        }
        "open" => {
            let name = string(&arguments, "name")?.context("open requires name")?;
            let topic = arguments
                .get("topic")
                .map(|value| value.as_str().context("topic must be a string"))
                .transpose()?
                .unwrap_or("");
            let context = client.self_context().await?;
            let value = client
                .create_channel(&CreateChannelReq {
                    name: name.to_string(),
                    topic: topic.to_string(),
                    repo_root: Some(context.repo_root),
                })
                .await?;
            super::structured_result(&format!("opened channel {}", value.id), &value)
        }
        "subscribe" => {
            let id = resolve_channel(&client, &arguments).await?;
            let mode = string(&arguments, "mode")?.unwrap_or("observe");
            let session = string(&arguments, "session")?;
            let value = client.set_channel_subscription(&id, mode, session).await?;
            super::structured_result(
                &format!("subscribed to channel {id} in {mode} mode"),
                &value,
            )
        }
        _ => unreachable!(),
    }
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_tools_use_resource_verbs() {
        let surface = tools();
        let names = surface
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, TOOL_NAMES);
        assert_eq!(expand_tool_set("mcp/channel/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("mcp/channel/write@v1").unwrap().len(), 4);
    }
}
