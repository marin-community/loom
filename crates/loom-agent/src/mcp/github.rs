//! Built-in MCP adapter for restricted GitHub sessions.
//!
//! All six tools call one registered operation,
//! `permissions.github.restricted.invoke`: the security boundary here is a
//! runtime check against the session's stored tool allowlist
//! (`session.policy_allowed_tools`), not a compile-time grant, so a restricted
//! session's policy — not its credential — decides which of these six tools
//! it may use. `super::dispatch::bind` assumes one tool name maps to one
//! operation; this is six tool names gated through one, so it keeps its own
//! schemas and dispatch loop instead.
//!
//! Claude sees these fixed tools instead of `Bash`. The bridge carries only the
//! session-scoped Loom token and forwards each call to Loom's REST API; the
//! GitHub credential remains in Loom's profile/user-token store and never enters
//! the adapter process.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use weaver_api::operations::permissions::github::restricted::{self, invoke};

use super::{Adapter, CapabilitySet, ServeFuture};

const SERVER_NAME: &str = "loom_github";
const COMMENT_TOOL_SET: &str = "mcp/github/comment";
const COMMENT_TOOL_SET_V1: &str = "mcp/github/comment@v1";
const LOOM_COMMENT_TOOL_SET_V1: &str = "loom/github/comment@v1";
const GITHUB_TOOL_NAMES: [&str; 6] = [
    restricted::TOOLS[0].name,
    restricted::TOOLS[1].name,
    restricted::TOOLS[2].name,
    restricted::TOOLS[3].name,
    restricted::TOOLS[4].name,
    restricted::TOOLS[5].name,
];

/// Six tool names gated through one operation, so there is no name-to-operation
/// pair to export.
fn no_exports() -> &'static [super::dispatch::Export] {
    &[]
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "github",
    server_name: SERVER_NAME,
    description: "Repository-scoped GitHub issue and pull-request operations.",
    capability_sets,
    exports: no_exports,
    superseded: &[("mcp/github/comment@v1", "loom/github/comment@v1")],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: LOOM_COMMENT_TOOL_SET_V1,
        group: "github",
        version: "v1",
        description: "Read, comment on, and edit the issue or pull request bound to the session.",
        tools: &GITHUB_TOOL_NAMES,
    },
    CapabilitySet {
        name: COMMENT_TOOL_SET_V1,
        group: "github",
        version: "v1",
        description: "Read, comment on, and edit the issue or pull request bound to the session.",
        tools: &GITHUB_TOOL_NAMES,
    },
];

fn capability_sets() -> &'static [CapabilitySet] {
    CAPABILITY_SETS
}

pub fn permission_rule(tool: &str) -> Option<String> {
    GITHUB_TOOL_NAMES
        .contains(&tool)
        .then(|| format!("mcp__{SERVER_NAME}__{tool}"))
}

fn is_permission_rule(rule: &str) -> bool {
    rule.strip_prefix("mcp__")
        .and_then(|suffix| suffix.split_once("__"))
        .is_some_and(|(server, tool)| server == SERVER_NAME && GITHUB_TOOL_NAMES.contains(&tool))
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    (matches!(
        name,
        COMMENT_TOOL_SET | COMMENT_TOOL_SET_V1 | LOOM_COMMENT_TOOL_SET_V1
    ))
    .then(|| {
        GITHUB_TOOL_NAMES
            .iter()
            .map(|tool| permission_rule(tool).expect("registered GitHub tool"))
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
    Value::Array(
        restricted::TOOLS
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.summary,
                    "inputSchema": (tool.schema)(),
                })
            })
            .collect(),
    )
}

fn result(id: &Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !GITHUB_TOOL_NAMES.contains(&name) {
        anyhow::bail!("unknown GitHub tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        anyhow::bail!("GitHub tool '{name}' is not allowed by this session");
    }
    let session_id =
        std::env::var("LOOM_SESSION_ID").context("restricted MCP is missing LOOM_SESSION_ID")?;
    let token = weaver_api::endpoint::token_from_env()
        .context("restricted MCP is missing its session-scoped LOOM_TOKEN")?;
    // The session and the tool are operands, not path segments, so neither is
    // percent-encoded: they travel in the JSON body.
    let view = weaver_api::Client::new(weaver_api::endpoint::base_url())
        .with_token(Some(token))
        .invoke::<invoke::Op>(&invoke::Input {
            tool: name.to_string(),
            arguments,
            session: session_id,
        })
        .await?;
    Ok(json!({
        "content": [{ "type": "text", "text": view.text }],
        "isError": false
    }))
}

async fn dispatch(request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    Some(match method {
        "initialize" => {
            let requested = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            result(
                &id,
                json!({
                    "protocolVersion": requested,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") }
                }),
            )
        }
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

/// Serve newline-delimited MCP JSON-RPC on stdin/stdout until the adapter
/// closes the pipe. Notifications deliberately receive no response.
async fn serve() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)
            .map_err(|error| anyhow!("invalid MCP JSON-RPC request: {error}"))?;
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
    use super::{expand_tool_set, permission_rule, server_config, tools, GITHUB_TOOL_NAMES};

    #[test]
    fn surface_contains_only_fixed_github_operations() {
        let surface = tools();
        let names: Vec<&str> = surface
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, GITHUB_TOOL_NAMES);
    }

    #[test]
    fn comment_set_expands_to_the_fixed_surface() {
        let expanded = expand_tool_set("mcp/github/comment@v1").unwrap();
        assert_eq!(expanded.len(), GITHUB_TOOL_NAMES.len());
        assert_eq!(expanded[0], permission_rule(GITHUB_TOOL_NAMES[0]).unwrap());
        assert!(expand_tool_set("mcp/github/admin").is_none());
    }

    #[test]
    fn registry_launches_the_generic_adapter_command() {
        let config = server_config();
        assert_eq!(
            config["args"],
            serde_json::json!(["mcp", "serve", "github"])
        );
    }
}
