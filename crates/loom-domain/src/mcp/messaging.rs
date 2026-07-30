//! Built-in session messaging MCP adapter.
//!
//! Both operations are facades over Loom's existing session-scoped REST
//! routes. Credentials, branch/thread routing, durable status events, and
//! GitHub/Slack mirroring remain server-side.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{Adapter, CapabilitySet, ServeFuture};

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
                .map(|tool| permission_rule(tool).expect("registered messaging tool"))
                .collect()
        })
}

fn server_config() -> Value {
    json!({
        "type": "stdio",
        "command": "loom",
        "args": ["mcp", "serve", ADAPTER.name]
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(serve())
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
            "description": "Post a message to the Slack thread fixed to this session.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "text": { "type": "string", "minLength": 1, "maxLength": 4000 }
                },
                "required": ["text"]
            }
        }
    ])
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown messaging tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("messaging tool '{name}' is not allowed by this session");
    }
    let session_id =
        std::env::var("LOOM_SESSION_ID").context("messaging MCP is missing LOOM_SESSION_ID")?;
    let token = weaver_api::endpoint::token_from_env()
        .context("messaging MCP is missing its session-scoped LOOM_TOKEN")?;
    let client = weaver_api::Client::new(weaver_api::endpoint::base_url()).with_token(Some(token));
    let session = client
        .get_session(&session_id)
        .await
        .context("resolving the messaging MCP session")?;
    let text = match name {
        "status_update" => {
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
            client
                .set_branch_status(&session.branch.id, level, message)
                .await?;
            format!("status updated to {level}")
        }
        "slack_reply" => {
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .context("slack_reply requires text")?;
            if text.is_empty() || text.len() > 4000 {
                bail!("slack_reply text must contain 1 to 4000 bytes");
            }
            client
                .post(
                    &format!(
                        "/api/branches/{}/slack/reply",
                        percent_encoding::utf8_percent_encode(
                            &session.branch.id,
                            percent_encoding::NON_ALPHANUMERIC
                        )
                    ),
                    json!({ "text": text }),
                )
                .await?;
            "message posted to the session Slack thread".to_string()
        }
        _ => unreachable!(),
    };
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    }))
}

fn result(id: &Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

async fn dispatch(request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    Some(match method {
        "initialize" => result(
            &id,
            json!({
                "protocolVersion": request.pointer("/params/protocolVersion")
                    .and_then(Value::as_str).unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "ping" => result(&id, json!({})),
        "tools/list" => result(&id, json!({ "tools": super::runtime_tools(tools()) })),
        "tools/call" => {
            let name = request.pointer("/params/name").and_then(Value::as_str);
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match name {
                Some(name) => match call_tool(name, arguments).await {
                    Ok(value) => result(&id, value),
                    Err(err) => result(
                        &id,
                        json!({
                            "content": [{ "type": "text", "text": format!("{err:#}") }],
                            "isError": true
                        }),
                    ),
                },
                None => error(&id, -32602, "tools/call requires params.name"),
            }
        }
        _ => error(&id, -32601, format!("method not found: {method}")),
    })
}

async fn serve() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value =
            serde_json::from_str(&line).map_err(|error| anyhow!("invalid MCP request: {error}"))?;
        if let Some(response) = dispatch(request).await {
            stdout
                .write_all(serde_json::to_string(&response)?.as_bytes())
                .await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
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
}
