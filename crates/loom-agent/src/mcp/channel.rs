//! Durable channel operations, served by the generic registry dispatcher.
//!
//! Every tool here is a registered `channels.*` operation: `tools/list` and
//! `tools/call` are both read off [`exports`]. Resolving `channel == "self"`,
//! bounding `read`'s scan window, and running `wait`'s poll loop belong in the
//! operation handler, not here: see `channels.messages.list` and
//! `channels.wait` in
//! `crates/loom/src/web/channels.rs`.
//!
//! `list`/`get` responses include delivery bindings via `ChannelView::bindings`.

use std::sync::OnceLock;

use serde_json::Value;

use weaver_api::operations::channels;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};

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

pub(super) const ADAPTER: Adapter = Adapter {
    name: "channel",
    description: "Durable conversation streams, subscriptions, and delivery receipts.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

/// Capability sets are derived from the registry: every `channels.*` operation
/// exported to this server contributes its tool to the set named by its grant.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| super::dispatch::capability_sets(exports(), "channel", describe_capability))
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/channels/read@v1" => "List, inspect, read, and wait on visible durable channels.",
        "loom/channels/write@v1" => "Send, acknowledge, open, and subscribe to durable channels.",
        _ => "Durable channel operations.",
    }
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("channel", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("channel", exports(), &name, arguments).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_sets_are_grouped_by_grant() {
        assert_eq!(expand_tool_set("loom/channels/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("loom/channels/write@v1").unwrap().len(), 4);
        assert!(expand_tool_set("mcp/channel/read@v1").is_none());
        assert!(expand_tool_set("loom/channels/nonexistent@v1").is_none());
    }
}
