//! Durable channel operations, served by the generic registry dispatcher.
//!
//! Every tool here is a registered `channels.*` operation — `tools/list` is
//! `weaver_api::mcp_tools_ordered(SERVER_NAME, TOOL_NAMES)` and `tools/call` is
//! `super::dispatch::call_tool`. There is no `project_input`/`present` pair
//! left to maintain: those existed to patch around per-tool argument shaping
//! (resolving `channel == "self"`, bounding `read`'s scan window, running
//! `wait`'s poll loop client-side) and each made its own `self_context()`
//! round-trip, all of which the operations themselves now do server-side —
//! see `channels.messages.list` and `channels.wait` in
//! `crates/loom/src/web/channels.rs`.
//!
//! `list`/`get` responses include delivery bindings via `ChannelView::bindings`.

use std::sync::OnceLock;

use serde_json::Value;

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_channel";
const TOOL_NAMES: [&str; 8] = [
    "list",
    "get",
    "read",
    "send",
    "wait",
    "ack",
    "open",
    "subscribe",
];

// Existing pinned sessions keep their exact identities. The `mcp/channel/*@v1`
// capability sets are hand-authored because no operation's grants field names them.
const LEGACY_READ_TOOLS: &[&str] = &["list", "get", "read", "wait"];
const LEGACY_WRITE_TOOLS: &[&str] = &["send", "ack", "open", "subscribe"];
const LEGACY_CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "mcp/channel/read@v1",
        group: "channel",
        version: "v1",
        description: "List, inspect, read, and wait on visible durable channels.",
        tools: LEGACY_READ_TOOLS,
    },
    CapabilitySet {
        name: "mcp/channel/write@v1",
        group: "channel",
        version: "v1",
        description: "Send, acknowledge, open, and subscribe to durable channels.",
        tools: LEGACY_WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "channel",
    server_name: SERVER_NAME,
    description: "Durable conversation streams, subscriptions, and delivery receipts.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

/// Capability sets are derived from the registry: every `channels.*` operation
/// whose MCP projection targets this server contributes its tool to the set named
/// by its grant. The `mcp/channel/*@v1` names are hand-authored because no
/// operation's grants field names them.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets =
            super::dispatch::derive_capability_sets(SERVER_NAME, "channel", describe_capability);
        sets.extend(LEGACY_CAPABILITY_SETS.iter().map(|set| CapabilitySet {
            name: set.name,
            group: set.group,
            version: set.version,
            description: set.description,
            tools: set.tools,
        }));
        sets
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/channels/read@v1" => "List, inspect, read, and wait on visible durable channels.",
        "loom/channels/write@v1" => "Send, acknowledge, open, and subscribe to durable channels.",
        _ => "Durable channel operations.",
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
    super::builtin_server_config("channel")
}

fn tools() -> Value {
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("channel", SERVER_NAME, &name, arguments).await
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_surface_is_derived_from_operation_descriptors() {
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
    fn capability_sets_are_grouped_by_grant() {
        assert_eq!(expand_tool_set("loom/channels/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("loom/channels/write@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("mcp/channel/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("mcp/channel/write@v1").unwrap().len(), 4);
        assert!(expand_tool_set("loom/channels/nonexistent@v1").is_none());
    }

    #[test]
    fn permission_rules_only_recognize_registered_tools() {
        assert!(is_permission_rule("mcp__loom_channel__list"));
        assert!(!is_permission_rule("mcp__loom_channel__bogus"));
        assert!(!is_permission_rule("mcp__other_server__list"));
    }
}
