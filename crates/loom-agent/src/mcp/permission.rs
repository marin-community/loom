//! Effective access and approval requests, served by the generic registry
//! dispatcher.
//!
//! Every tool here is a registered `permissions.*` operation — `tools/list`
//! is `weaver_api::mcp_tools_ordered(SERVER_NAME, TOOL_NAMES)` and
//! `tools/call` is `super::dispatch::call_tool`. There is no
//! `project_input`/`present` pair left to maintain: those existed to patch
//! around per-tool response framing (a custom one-line summary per tool),
//! which the generic dispatcher's default `Render` (the operation's own JSON)
//! now provides. `permissions.requests.approve`/`.deny` stay unreachable here
//! because they are `actor = User`, not `SessionSelf` — an MCP projection on
//! a non-agent-reachable operation is rejected by the registry itself, so
//! "an agent cannot approve its own permission request" needs no adapter-side
//! enforcement.

use std::sync::OnceLock;

use serde_json::Value;

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_permission";
const TOOL_NAMES: [&str; 4] = ["show", "explain", "requests", "request"];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "permission",
    server_name: SERVER_NAME,
    description: "Effective operation grants and durable human approval requests.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

/// Capability sets are derived from the registry: every `permissions.*` operation
/// whose MCP projection targets this server contributes its tool to the set named
/// by its grant.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        super::dispatch::derive_capability_sets(SERVER_NAME, "permissions", describe_capability)
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/permissions/read@v1" => {
            "Inspect effective Loom operations, repository scope, and access requests."
        }
        "loom/permissions/request@v1" => {
            "Request a human-approved expansion of external session access."
        }
        _ => "Effective operation grants and durable human approval requests.",
    }
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    capability_sets()
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
    super::builtin_server_config("permission")
}

fn tools() -> Value {
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("permission", SERVER_NAME, &name, arguments).await
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_surface_is_derived_from_operation_descriptors() {
        let names = tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, TOOL_NAMES);
        for name in &names {
            assert!(weaver_api::operation_for_mcp(SERVER_NAME, name).is_some());
        }
    }

    #[test]
    fn request_capability_remains_separate_from_human_decision() {
        assert_eq!(
            expand_tool_set("loom/permissions/request@v1")
                .unwrap()
                .len(),
            1
        );
        assert!(weaver_api::operation("permissions.requests.approve")
            .unwrap()
            .mcp
            .is_none());
    }

    #[test]
    fn permission_rules_only_recognize_registered_tools() {
        assert!(is_permission_rule("mcp__loom_permission__show"));
        assert!(!is_permission_rule("mcp__loom_permission__bogus"));
        assert!(!is_permission_rule("mcp__other_server__show"));
    }
}
