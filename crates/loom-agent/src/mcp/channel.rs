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
        name: "loom/channels/read@v1",
        group: "channel",
        version: "v1",
        description: "List, inspect, read, and wait on visible durable channels.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "loom/channels/write@v1",
        group: "channel",
        version: "v1",
        description: "Send, acknowledge, open, and subscribe to durable channels.",
        tools: WRITE_TOOLS,
    },
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

fn is_permission_rule(rule: &str) -> bool {
    super::is_builtin_permission_rule(SERVER_NAME, &TOOL_NAMES, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::expand_builtin_tool_set(SERVER_NAME, &TOOL_NAMES, CAPABILITY_SETS, name)
}

fn server_config() -> Value {
    super::builtin_server_config("channel")
}

fn tools() -> Value {
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

fn object(arguments: &Value) -> Result<&Map<String, Value>> {
    arguments
        .as_object()
        .context("channel tool arguments must be an object")
}

async fn resolve_channel(client: &weaver_api::Client, arguments: &Value) -> Result<String> {
    match super::string_argument(arguments, "channel")? {
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
            if !(1..=weaver_api::CHANNEL_MESSAGE_LIMIT_MAX as u64).contains(&limit) {
                bail!(
                    "limit must be between 1 and {}",
                    weaver_api::CHANNEL_MESSAGE_LIMIT_MAX
                );
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
            let fetched = client
                .channel_messages_bounded(&id, after, weaver_api::CHANNEL_MESSAGE_LIMIT_MAX)
                .await?;
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
            let body = super::string_argument(&arguments, "body")?.context("send requires body")?;
            let request = CreateChannelMessageReq {
                kind: super::string_argument(&arguments, "kind")?
                    .unwrap_or("message")
                    .to_string(),
                urgency: super::string_argument(&arguments, "urgency")?
                    .unwrap_or("normal")
                    .to_string(),
                body: body.to_string(),
                payload: arguments
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                reply_to: super::string_argument(&arguments, "reply_to")?.map(str::to_string),
                idempotency_key: super::string_argument(&arguments, "idempotency_key")?
                    .map(str::to_string),
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
            let kind = super::string_argument(&arguments, "kind")?;
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
                let messages = client
                    .channel_messages_bounded(&id, cursor, weaver_api::CHANNEL_MESSAGE_LIMIT_MAX)
                    .await?;
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
            let name = super::string_argument(&arguments, "name")?.context("open requires name")?;
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
            let mode = super::string_argument(&arguments, "mode")?.unwrap_or("observe");
            let session = super::string_argument(&arguments, "session")?;
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
