//! Durable channel operations, served by the generic registry dispatcher.
//!
//! Every tool here is a registered `channels.*` operation: `tools/list` is
//! `weaver_api::mcp_tools_ordered(SERVER_NAME, TOOL_NAMES)`, and `tools/call`
//! is `super::dispatch::call_tool`. Argument shaping — resolving
//! `channel == "self"`, bounding `read`'s scan window, running `wait`'s poll
//! loop — belongs in the operation handler, not here: see
//! `channels.messages.list` and `channels.wait` in
//! `crates/loom/src/web/channels.rs`.
//!
//! `list`/`get` responses include delivery bindings via `ChannelView::bindings`.

use std::sync::OnceLock;

use serde_json::Value;

use weaver_api::operations::channels;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_channel";

/// The tools this server exports, in the order it advertises them.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<channels::list::Op>("list"),
            export::<channels::get::Op>("get"),
            export::<channels::messages::list::Op>("read"),
            export::<channels::messages::create::Op>("send"),
            export::<channels::wait::Op>("wait"),
            export::<channels::read_marker::set::Op>("ack"),
            export::<channels::create::Op>("open"),
            export::<channels::subscription::set::Op>("subscribe"),
        ]
    })
}

// The names these sets carried before the `loom/` rename. Sessions pinned to
// one still resolve it.
const SUPERSEDED: &[(&str, &str)] = &[
    ("mcp/channel/read@v1", "loom/channels/read@v1"),
    ("mcp/channel/write@v1", "loom/channels/write@v1"),
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "channel",
    server_name: SERVER_NAME,
    description: "Durable conversation streams, subscriptions, and delivery receipts.",
    capability_sets,
    exports,
    superseded: &[
        ("mcp/channel/read@v1", "loom/channels/read@v1"),
        ("mcp/channel/write@v1", "loom/channels/write@v1"),
    ],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

/// Capability sets are derived from the registry: every `channels.*` operation
/// whose MCP projection targets this server contributes its tool to the set
/// named by its grant, plus the same sets under the names they were superseded
/// away from.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets = super::dispatch::capability_sets(exports(), "channel", describe_capability);
        sets.extend(super::dispatch::alias_capability_sets(&sets, SUPERSEDED));
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
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
}

fn server_config() -> Value {
    super::builtin_server_config("channel")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("channel", SERVER_NAME, exports(), &name, arguments)
            .await
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

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
