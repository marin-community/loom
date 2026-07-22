//! Built-in MCP adapter for restricted GitHub sessions.
//!
//! Claude sees these fixed tools instead of `Bash`. The bridge carries only the
//! session-scoped Loom token and forwards each call to Loom's REST API; the
//! GitHub credential remains in Loom's profile/user-token store and never enters
//! the adapter process.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{Adapter, ServeFuture};

const SERVER_NAME: &str = "loom_github";
const COMMENT_TOOL_SET: &str = "mcp/github/comment";
pub(crate) const BODY_MAX_BYTES: usize = 65_536;
pub(crate) const TITLE_MAX_BYTES: usize = 256;
const GITHUB_TOOL_NAMES: [&str; 6] = [
    "issue_view",
    "issue_comment",
    "issue_edit",
    "pr_view",
    "pr_comment",
    "pr_edit",
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "github",
    server_name: SERVER_NAME,
    expand_tool_set,
    is_permission_rule,
    server_config,
    serve: serve_boxed,
};

pub(crate) fn permission_rule(tool: &str) -> Option<String> {
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
    (name == COMMENT_TOOL_SET).then(|| {
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
        "args": ["mcp", ADAPTER.name]
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(serve())
}

fn tools() -> Value {
    let number = json!({ "type": "integer", "minimum": 1 });
    let body = json!({ "type": "string", "maxLength": BODY_MAX_BYTES });
    let title = json!({ "type": "string", "minLength": 1, "maxLength": TITLE_MAX_BYTES });
    json!([
        {
            "name": GITHUB_TOOL_NAMES[0],
            "description": "Read one issue in the GitHub repository fixed to this session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "number": number }, "required": ["number"]
            }
        },
        {
            "name": GITHUB_TOOL_NAMES[1],
            "description": "Post a comment on one issue in the GitHub repository fixed to this session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "number": number, "body": body },
                "required": ["number", "body"]
            }
        },
        {
            "name": GITHUB_TOOL_NAMES[2],
            "description": "Replace an issue body and optionally its title in the GitHub repository fixed to this session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "number": number, "body": body, "title": title },
                "required": ["number", "body"]
            }
        },
        {
            "name": GITHUB_TOOL_NAMES[3],
            "description": "Read one pull request in the GitHub repository fixed to this session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "number": number }, "required": ["number"]
            }
        },
        {
            "name": GITHUB_TOOL_NAMES[4],
            "description": "Post a comment on one pull request in the GitHub repository fixed to this session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "number": number, "body": body },
                "required": ["number", "body"]
            }
        },
        {
            "name": GITHUB_TOOL_NAMES[5],
            "description": "Replace a pull-request body and optionally its title in the GitHub repository fixed to this session.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "number": number, "body": body, "title": title },
                "required": ["number", "body"]
            }
        }
    ])
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
    let session_id =
        std::env::var("LOOM_SESSION_ID").context("restricted MCP is missing LOOM_SESSION_ID")?;
    let path = format!(
        "/api/sessions/{}/restricted-github/{}",
        percent_encoding::utf8_percent_encode(&session_id, percent_encoding::NON_ALPHANUMERIC),
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC),
    );
    let token = weaver_api::endpoint::token_from_env()
        .context("restricted MCP is missing its session-scoped LOOM_TOKEN")?;
    let value = weaver_api::Client::new(weaver_api::endpoint::base_url())
        .with_token(Some(token))
        .post(
            &path,
            serde_json::to_value(weaver_api::RestrictedGithubToolReq { arguments })?,
        )
        .await?;
    let view: weaver_api::RestrictedGithubToolView =
        serde_json::from_value(value).context("decoding restricted GitHub tool response")?;
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
        "tools/list" => result(&id, json!({ "tools": tools() })),
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
                            "content": [{ "type": "text", "text": err.to_string() }],
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
        let expanded = expand_tool_set("mcp/github/comment").unwrap();
        assert_eq!(expanded.len(), GITHUB_TOOL_NAMES.len());
        assert_eq!(expanded[0], permission_rule(GITHUB_TOOL_NAMES[0]).unwrap());
        assert!(expand_tool_set("mcp/github/admin").is_none());
    }

    #[test]
    fn registry_launches_the_generic_adapter_command() {
        let config = server_config();
        assert_eq!(config["args"], serde_json::json!(["mcp", "github"]));
    }
}
