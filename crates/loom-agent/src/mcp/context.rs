//! Current caller context, served by the generic registry dispatcher.
//!
//! The single tool here is the registered `sessions.context` operation
//! (`mcp = "loom_context::get"`) — `tools/list` is
//! `weaver_api::mcp_tools_ordered` and `tools/call` is
//! `super::dispatch::call_tool`. The old hand-rolled "accepts no arguments"
//! check is gone: the operation's `Input` has no caller-supplied fields, so
//! the generic dispatcher's schema-driven merge already leaves nothing for an
//! extra argument to do.
//!
//! Capability sets stay hand-authored rather than
//! `super::dispatch::derive_capability_sets`: `sessions.context`'s own
//! `grants` names `loom/sessions/read@v1` — it is part of the same read
//! grant as `loom_session`'s tools, just served from a different MCP process
//! — so deriving from it here would mint a *second*, incomplete
//! `loom/sessions/read@v1` set (only `get`, missing `summary`/`history`/etc.)
//! under `context`'s own adapter. `expand_tool_set` resolves a set name
//! against the *first* adapter that recognizes it
//! (`crate::mcp::expand_tool_sets`), so that collision would silently steal
//! the name from `session`'s real set for any caller that lists adapters in
//! this order — this adapter keeps its own distinct `loom/context/read@v1`
//! identity instead.

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
