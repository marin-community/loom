//! Explicit outbound messaging to a Slack thread routed to this session.

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::branches;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};

fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| vec![export::<branches::slack::send::Op>("slack_send")])
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "messaging",
    description: "Explicit outbound messages to routed external conversations.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        vec![CapabilitySet {
            name: "loom/messaging/slack@v1",
            group: "messaging",
            version: "v1",
            description: "Send a one-off message to a Slack thread routed to this session.",
            tools: &["slack_send"],
        }]
    })
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("messaging", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("messaging", exports(), &name, arguments).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_surface_is_explicit_and_canonical() {
        assert_eq!(capability_sets().len(), 1);
        assert_eq!(
            expand_tool_set("loom/messaging/slack@v1").unwrap(),
            vec!["mcp__loom__messaging_slack_send"]
        );
        assert_eq!(tools().as_array().unwrap().len(), 1);
    }
}
