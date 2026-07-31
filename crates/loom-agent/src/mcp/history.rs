//! Built-in session self-history MCP adapter.
//!
//! This is deliberately only a facade: the REST history resources own source
//! normalization, pagination, filtering, literal search, and authorization.
//! The tool surface has no session selector; it resolves `LOOM_SESSION_ID` and
//! calls the corresponding session route with the scoped token.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{Adapter, CapabilitySet, ServeFuture};

const SERVER_NAME: &str = "loom_history";
const TOOL_NAMES: [&str; 2] = ["history", "search"];
const HISTORY_TOOLS: &[&str] = &TOOL_NAMES;
const CAPABILITY_SETS: &[CapabilitySet] = &[CapabilitySet {
    name: "mcp/history/self@v1",
    group: "history",
    version: "v1",
    description: "Page and literally search the normalized history of this session.",
    tools: HISTORY_TOOLS,
}];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "history",
    server_name: SERVER_NAME,
    description: "Session-scoped normalized history and literal search.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

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
                .map(|tool| permission_rule(tool).expect("registered history tool"))
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
    let common = json!({
        "before": {
            "type": "string",
            "minLength": 1,
            "description": "Exclusive opaque cursor returned as older_cursor by the preceding page."
        },
        "limit": {
            "type": "integer",
            "minimum": 1,
            "maximum": crate::history::MAX_LIMIT
        },
        "kinds": {
            "type": "array",
            "items": { "type": "string", "enum": crate::history::KINDS },
            "uniqueItems": true,
            "description": "Optional normalized record-kind filter."
        }
    });
    json!([
        {
            "name": "history",
            "description": "Page normalized records for this session. Records are chronological within each newest-tail page; follow older_cursor backward. Tool input is present only when the source transcript supplied it.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": common
            }
        },
        {
            "name": "search",
            "description": "Case-insensitive literal search over this session's normalized records. Uses the same paging cursor and optional kind filters as history.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "q": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": crate::history::MAX_QUERY_BYTES
                    },
                    "before": common["before"],
                    "limit": common["limit"],
                    "kinds": common["kinds"]
                },
                "required": ["q"]
            }
        }
    ])
}

fn optional_args(arguments: &Value) -> Result<(Option<&str>, Option<usize>, Vec<String>)> {
    let before = match arguments.get("before") {
        Some(value) => Some(
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .context("before must be a non-empty string")?,
        ),
        None => None,
    };
    let limit = match arguments.get("limit") {
        Some(value) => Some(
            usize::try_from(value.as_u64().context("limit must be a positive integer")?)
                .context("limit is too large")?,
        ),
        None => None,
    };
    let kinds = match arguments.get("kinds") {
        Some(value) => value
            .as_array()
            .context("kinds must be an array")?
            .iter()
            .map(|kind| {
                kind.as_str()
                    .context("every kind must be a string")
                    .map(str::to_string)
            })
            .collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    Ok((before, limit, kinds))
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown history tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("history tool '{name}' is not allowed by this session");
    }
    let allowed = match name {
        "history" => &["before", "limit", "kinds"][..],
        "search" => &["q", "before", "limit", "kinds"][..],
        _ => unreachable!(),
    };
    let object = arguments
        .as_object()
        .context("history tool arguments must be an object")?;
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        bail!("unknown argument '{unknown}' for history tool '{name}'");
    }
    let session_id =
        std::env::var("LOOM_SESSION_ID").context("history MCP is missing LOOM_SESSION_ID")?;
    let token = weaver_api::endpoint::token_from_env()
        .context("history MCP is missing its session-scoped LOOM_TOKEN")?;
    let client = weaver_api::Client::new(weaver_api::endpoint::base_url()).with_token(Some(token));
    let (before, limit, kinds) = optional_args(&arguments)?;
    let page = match name {
        "history" => {
            client
                .get_session_history(&session_id, before, limit, &kinds)
                .await?
        }
        "search" => {
            let query = arguments
                .get("q")
                .and_then(Value::as_str)
                .context("search requires q")?;
            client
                .search_session_history(&session_id, query, before, limit, &kinds)
                .await?
        }
        _ => unreachable!(),
    };
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&page)?
        }],
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
    fn self_history_surface_has_no_session_selector() {
        let surface = tools();
        for tool in surface.as_array().unwrap() {
            assert!(tool["inputSchema"]["properties"]
                .get("session_id")
                .is_none());
        }
        assert_eq!(
            expand_tool_set("mcp/history/self@v1").unwrap(),
            vec!["mcp__loom_history__history", "mcp__loom_history__search"]
        );
    }

    #[tokio::test]
    async fn unadvertised_session_selector_fails_closed() {
        let error = call_tool("history", json!({ "session_id": "sibling" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown argument 'session_id'"));
    }
}
