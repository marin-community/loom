//! Current caller context, served by the generic registry dispatcher.
//!
//! The single tool here is the registered `sessions.context` operation
//! (`mcp = "loom_context::get"`) — `tools/list` is
//! `weaver_api::mcp_tools_ordered` and `tools/call` is
//! `super::dispatch::call_tool`. The schema-driven argument merge handles all
//! required shape; no extra validation is needed.
//!
//! Capability sets are hand-authored to avoid a name collision. The
//! `sessions.context` operation claims the same `loom/sessions/read@v1` grant
//! as `loom_session`'s read tools, but serves from a different MCP process. If
//! this adapter derived its sets, `expand_tool_set` would return the first match
//! and silently hide the real read set. Instead, this adapter keeps a distinct
//! `loom/context/read@v1` identity.

use serde_json::Value;

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_context";
const TOOL_NAMES: [&str; 1] = ["get"];
const TOOLS: &[&str] = &TOOL_NAMES;
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "loom/context/read@v1",
        group: "context",
        version: "v1",
        description: "Resolve this session's canonical Loom resource identifiers and links.",
        tools: TOOLS,
    },
    CapabilitySet {
        name: "mcp/context/read@v1",
        group: "context",
        version: "v1",
        description: "Resolve this session's canonical Loom resource identifiers and links.",
        tools: TOOLS,
    },
];

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
    super::dispatch::is_permission_rule(SERVER_NAME, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    CAPABILITY_SETS
        .iter()
        .find(|set| set.name == name)
        .map(|set| {
            set.tools
                .iter()
                .map(|tool| format!("mcp__{SERVER_NAME}__{tool}"))
                .collect()
        })
}

fn server_config() -> Value {
    super::builtin_server_config("context")
}

fn tools() -> Value {
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("context", SERVER_NAME, &name, arguments).await
    })
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
        assert!(weaver_api::operation_for_mcp(SERVER_NAME, "get").is_some());
        assert_eq!(
            expand_tool_set("mcp/context/read@v1").unwrap(),
            vec!["mcp__loom_context__get"]
        );
        assert_eq!(
            expand_tool_set("loom/context/read@v1").unwrap(),
            vec!["mcp__loom_context__get"]
        );
    }
}
