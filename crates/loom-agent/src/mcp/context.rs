//! Current caller context as a small, typed REST projection.

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_context";
const TOOL_NAMES: [&str; 1] = ["get"];
const TOOLS: &[&str] = &TOOL_NAMES;
const CAPABILITY_SETS: &[CapabilitySet] = &[CapabilitySet {
    name: "mcp/context/read@v1",
    group: "context",
    version: "v1",
    description: "Resolve this session's canonical Loom resource identifiers and links.",
    tools: TOOLS,
}];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "context",
    server_name: SERVER_NAME,
    description: "Current session, branch, repository, channel, and resource links.",
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
    super::builtin_server_config("context")
}

fn tools() -> Value {
    json!([{
        "name": "get",
        "description": "Return this caller's session, branch, repository, default channel, dashboard URL, and canonical REST links.",
        "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} }
    }])
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if name != "get" {
        bail!("unknown context tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("context tool '{name}' is not allowed by this session");
    }
    if !arguments.as_object().is_some_and(|value| value.is_empty()) {
        bail!("context get accepts no arguments");
    }
    let value = super::runtime_client("context")?.self_context().await?;
    super::structured_result("resolved current Loom context", &value)
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_surface_is_one_read_only_tool() {
        assert_eq!(tools().as_array().unwrap().len(), 1);
        assert_eq!(
            expand_tool_set("mcp/context/read@v1").unwrap(),
            vec!["mcp__loom_context__get"]
        );
    }
}
