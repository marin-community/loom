//! Built-in session messaging MCP adapter.

use anyhow::Result;
use serde_json::Value;

use std::sync::OnceLock;

use weaver_api::operations::{branches, sessions};

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_messaging";

/// The tools this server exports, in the order it advertises them.
///
/// `status_update` is `loom_session::status_set` under another name. The
/// dispatcher checks the allow-list against the name it is handed, so serving
/// it here needs no separate handler.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<sessions::status::set::Op>("status_update"),
            export::<branches::slack::reply::Op>("slack_reply"),
        ]
    })
}

// Both sets are named by this adapter: `status_update` shares a grant with
// `loom_session`'s write tool, and `expand_tool_set` resolves a name to the
// first adapter that recognizes it.
const STATUS_SET: &str = "mcp/messaging/status@v1";
const SLACK_SET: &str = "mcp/slack/message@v1";

pub(super) const ADAPTER: Adapter = Adapter {
    name: "messaging",
    server_name: SERVER_NAME,
    description: "Session status and fixed-thread messaging through Loom routing.",
    capability_sets,
    exports,
    superseded: &[("mcp/messaging/status@v1", "loom/sessions/write@v1")],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        exports()
            .iter()
            .map(|export| {
                let (name, description) = match export.tool {
                    "status_update" => (
                        STATUS_SET,
                        "Update the durable Weaver status and its configured mirrors.",
                    ),
                    _ => (
                        SLACK_SET,
                        "Post a message to the Slack thread fixed to this session.",
                    ),
                };
                CapabilitySet {
                    name,
                    group: "messaging",
                    version: "v1",
                    description,
                    tools: Vec::leak(vec![export.tool]),
                }
            })
            .collect()
    })
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
}

fn server_config() -> Value {
    super::builtin_server_config("messaging")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    super::dispatch::call_adapter_tool("messaging", SERVER_NAME, exports(), name, arguments).await
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messaging_sets_are_grouped_and_exact() {
        assert_eq!(capability_sets().len(), 2);
        assert!(capability_sets().iter().all(|set| set.group == "messaging"));
        assert_eq!(
            expand_tool_set("mcp/messaging/status@v1").unwrap(),
            vec!["mcp__loom_messaging__status_update"]
        );
        assert_eq!(tools().as_array().unwrap().len(), 2);
    }
}
