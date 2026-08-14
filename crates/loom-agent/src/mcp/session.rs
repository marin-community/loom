//! Session lifecycle and normalized history projected from Loom's REST API.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_session";
const TOOL_NAMES: [&str; 4] = ["get", "status", "history", "search"];
const READ_TOOLS: &[&str] = &["get", "history", "search"];
const WRITE_TOOLS: &[&str] = &["status"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "mcp/session/read@v1",
        group: "session",
        version: "v1",
        description: "Inspect visible sessions and normalized session history.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "mcp/session/status@v1",
        group: "session",
        version: "v1",
        description: "Update this session's durable status projection and status stream.",
        tools: WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "session",
    server_name: SERVER_NAME,
    description: "Session lifecycle, status projection, and normalized history.",
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
                .map(|tool| permission_rule(tool).expect("registered session tool"))
                .collect()
        })
}

fn server_config() -> Value {
    json!({ "type": "stdio", "command": "loom", "args": ["mcp", "serve", "session"] })
}

fn session_property() -> Value {
    json!({
        "type": "string", "minLength": 1,
        "description": "A visible session id. Omit or pass 'self' for this session."
    })
}

fn history_properties() -> Value {
    json!({
        "before": { "type": "string", "minLength": 1 },
        "limit": { "type": "integer", "minimum": 1, "maximum": crate::history::MAX_LIMIT },
        "kinds": { "type": "array", "uniqueItems": true, "items": {
            "type": "string", "enum": crate::history::KINDS
        }}
    })
}

fn tools() -> Value {
    let history = history_properties();
    json!([
        {
            "name": "get",
            "description": "Get one visible session and its branch/lifecycle metadata.",
            "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
                "session": session_property()
            }}
        },
        {
            "name": "status",
            "description": "Update this session's status projection and append a typed status item to its channel.",
            "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
                "level": { "type": "string", "enum": ["ok", "attention", "blocked"] },
                "message": { "type": "string", "maxLength": 4096 }
            }, "required": ["level", "message"] }
        },
        {
            "name": "history",
            "description": "Page normalized records for one visible session, newest tail first.",
            "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
                "session": session_property(),
                "before": history["before"], "limit": history["limit"], "kinds": history["kinds"]
            }}
        },
        {
            "name": "search",
            "description": "Case-insensitive literal search over one visible session's normalized history.",
            "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
                "session": session_property(),
                "q": { "type": "string", "minLength": 1, "maxLength": crate::history::MAX_QUERY_BYTES },
                "before": history["before"], "limit": history["limit"], "kinds": history["kinds"]
            }, "required": ["q"] }
        }
    ])
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

async fn resolve_session(client: &weaver_api::Client, arguments: &Value) -> Result<String> {
    match string(arguments, "session")? {
        Some(session) if session != "self" => Ok(session.to_string()),
        _ => Ok(client.self_context().await?.session_id),
    }
}

fn history_args(arguments: &Value) -> Result<(Option<&str>, Option<usize>, Vec<String>)> {
    let before = string(arguments, "before")?;
    let limit = arguments
        .get("limit")
        .map(|value| {
            let limit = value.as_u64().context("limit must be a positive integer")?;
            let limit = usize::try_from(limit).context("limit is too large")?;
            (limit > 0 && limit <= crate::history::MAX_LIMIT)
                .then_some(limit)
                .context("limit is outside the supported range")
        })
        .transpose()?;
    let kinds = arguments
        .get("kinds")
        .map(|value| {
            value
                .as_array()
                .context("kinds must be an array")?
                .iter()
                .map(|kind| {
                    kind.as_str()
                        .context("every kind must be a string")
                        .map(str::to_string)
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok((before, limit, kinds))
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown session tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("session tool '{name}' is not allowed by this session");
    }
    arguments
        .as_object()
        .context("session tool arguments must be an object")?;
    let client = super::runtime_client("session")?;
    match name {
        "get" => {
            let id = resolve_session(&client, &arguments).await?;
            let session = client.get_session(&id).await?;
            super::structured_result(&format!("session {id}"), &session)
        }
        "status" => {
            let level = string(&arguments, "level")?.context("status requires level")?;
            if !matches!(level, "ok" | "attention" | "blocked") {
                bail!("level must be 'ok', 'attention', or 'blocked'");
            }
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .context("status requires string message")?;
            if message.len() > 4096 {
                bail!("status message must be at most 4096 bytes");
            }
            let context = client.self_context().await?;
            let branch = client
                .set_branch_status(&context.branch_id, level, message)
                .await?;
            let channel = client.get_channel(&context.channel_id).await?;
            let value = json!({ "branch": branch, "status_message": channel.last_message });
            super::structured_result(&format!("status updated to {level}"), &value)
        }
        "history" | "search" => {
            let id = resolve_session(&client, &arguments).await?;
            let (before, limit, kinds) = history_args(&arguments)?;
            let page = if name == "history" {
                client
                    .get_session_history(&id, before, limit, &kinds)
                    .await?
            } else {
                let query = string(&arguments, "q")?.context("search requires q")?;
                client
                    .search_session_history(&id, query, before, limit, &kinds)
                    .await?
            };
            super::structured_result(
                &format!("{} normalized history record(s)", page.records.len()),
                &page,
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
    fn session_tools_consolidate_status_and_history() {
        let surface = tools();
        let names = surface
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, TOOL_NAMES);
        assert_eq!(expand_tool_set("mcp/session/read@v1").unwrap().len(), 3);
    }
}
