//! This session's own normalized history.

use std::sync::OnceLock;

use anyhow::Result;
use serde_json::Value;
use weaver_api::operations::sessions;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_history";

/// The tools this server exports, in the order it advertises them.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<sessions::history::list::Op>("history"),
            export::<sessions::history::search::Op>("search"),
        ]
    })
}

// Both exports claim `loom/sessions/read@v1`, the grant `loom_session`'s read
// tools also claim, and `expand_tool_set` resolves a set name to the first
// adapter that recognizes it — so this adapter names its own set.
const SET_NAME: &str = "mcp/history/self@v1";

pub(super) const ADAPTER: Adapter = Adapter {
    name: "history",
    server_name: SERVER_NAME,
    description: "Session-scoped normalized history and literal search.",
    capability_sets,
    exports,
    superseded: &[("mcp/history/self@v1", "loom/sessions/read@v1")],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets = super::dispatch::capability_sets(exports(), "history", |_| {
            "Page and literally search the normalized history of this session."
        });
        for set in &mut sets {
            set.name = SET_NAME;
        }
        sets
    })
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
}

fn server_config() -> Value {
    super::builtin_server_config("history")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    super::dispatch::call_adapter_tool("history", SERVER_NAME, exports(), name, arguments).await
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_history_surface_has_no_session_selector() {
        let surface = tools();
        for tool in surface.as_array().unwrap() {
            assert!(tool["inputSchema"]["properties"]
                .get("session_id")
                .is_none());
        }
        assert_eq!(
            expand_tool_set("mcp/history/self@v1").unwrap(),
            vec!["mcp__loom_history__history", "mcp__loom_history__search"]
        );
    }
}
