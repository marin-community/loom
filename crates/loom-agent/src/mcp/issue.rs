//! Repository work items, served by the generic registry dispatcher.
//!
//! Every tool here is a registered `issues.*` operation — `tools/list` is
//! `weaver_api::mcp_tools(SERVER_NAME)` and `tools/call` is
//! `super::dispatch::call_tool`. There is no `project_input`/`present` pair
//! left to maintain: those existed to patch around per-tool argument shaping
//! and each made its own `self_context()` round-trip, both of which the
//! generic dispatcher now does once, from the operation's own types.

use std::sync::OnceLock;

use serde_json::Value;

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_issue";

pub(super) const ADAPTER: Adapter = Adapter {
    name: "issue",
    server_name: SERVER_NAME,
    description: "Repository work items, lifecycle, and free-form tags.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

/// Capability sets, derived from the registry rather than hand-maintained:
/// every `issues.*` operation whose MCP projection targets this server
/// contributes its tool to the set named by its grant. Description text is
/// still authored here — the registry carries no prose for it.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        super::dispatch::derive_capability_sets(SERVER_NAME, "issue", describe_capability)
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/issues/read@v1" => "List and inspect work items in this session's repository.",
        "loom/issues/write@v1" => "Create, update, tag, and remove repository work items.",
        _ => "Repository work item operations.",
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
    super::builtin_server_config("issue")
}

fn tools() -> Value {
    weaver_api::mcp_tools(SERVER_NAME)
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        let client = super::runtime_client("issue")?;
        super::dispatch::call_tool(&client, SERVER_NAME, &name, arguments).await
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_surface_is_derived_from_operation_descriptors() {
        let names = tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "actions",
                "add",
                "backlog_add",
                "close",
                "delete",
                "get",
                "list",
                "reopen",
                "tag_delete",
                "tag_set",
            ]
        );
        for name in &names {
            assert!(weaver_api::operation_for_mcp(SERVER_NAME, name).is_some());
        }
    }

    #[test]
    fn capability_sets_are_grouped_by_grant() {
        assert_eq!(expand_tool_set("loom/issues/read@v1").unwrap().len(), 2);
        assert_eq!(expand_tool_set("loom/issues/write@v1").unwrap().len(), 8);
        assert!(expand_tool_set("loom/issues/nonexistent@v1").is_none());
    }

    #[test]
    fn permission_rules_only_recognize_registered_tools() {
        assert!(is_permission_rule("mcp__loom_issue__list"));
        assert!(!is_permission_rule("mcp__loom_issue__bogus"));
        assert!(!is_permission_rule("mcp__other_server__list"));
    }
}
