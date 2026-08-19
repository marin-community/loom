//! Built-in session self-history MCP adapter.
//!
//! `history` and `search` route to `sessions.history.*` operations under the
//! `loom_session` server because no operation declares an `loom_history::*`
//! projection. The session selector is still refused because `session` is a
//! context field that the schema never advertises.
//!
//! The tool surface is hand-written because `sessions.history.*`'s registry
//! schema lacks the `limit`/`kinds`/`q` bounds this capability set advertises.

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_history";
const TOOL_NAMES: [&str; 2] = ["history", "search"];
const HISTORY_TOOLS: &[&str] = &TOOL_NAMES;
const CAPABILITY_SETS: &[CapabilitySet] = &[CapabilitySet {
    name: "mcp/history/self@v1",
    group: "history",
    version: "v1",
    description: "Page and literally search the normalized history of this session.",
    tools: HISTORY_TOOLS,
}];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "history",
    server_name: SERVER_NAME,
    description: "Session-scoped normalized history and literal search.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    CAPABILITY_SETS
}

fn is_permission_rule(rule: &str) -> bool {
    super::is_builtin_permission_rule(SERVER_NAME, &TOOL_NAMES, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::expand_builtin_tool_set(SERVER_NAME, &TOOL_NAMES, CAPABILITY_SETS, name)
}

fn server_config() -> Value {
    super::builtin_server_config("history")
}

fn tools() -> Value {
    let common = json!({
        "before": {
            "type": "string",
            "minLength": 1,
            "description": "Exclusive opaque cursor returned as older_cursor by the preceding page."
        },
        "limit": {
            "type": "integer",
            "minimum": 1,
            "maximum": crate::history::MAX_LIMIT
        },
        "kinds": {
            "type": "array",
            "items": { "type": "string", "enum": crate::history::KINDS },
            "uniqueItems": true,
            "description": "Optional normalized record-kind filter."
        }
    });
    json!([
        {
            "name": "history",
            "description": "Page normalized records for this session. Records are chronological within each newest-tail page; follow older_cursor backward. Tool input is present only when the source transcript supplied it.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": common
            }
        },
        {
            "name": "search",
            "description": "Case-insensitive literal search over this session's normalized records. Uses the same paging cursor and optional kind filters as history.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "q": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": crate::history::MAX_QUERY_BYTES
                    },
                    "before": common["before"],
                    "limit": common["limit"],
                    "kinds": common["kinds"]
                },
                "required": ["q"]
            }
        }
    ])
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown history tool '{name}'");
    }
    let client = super::runtime_client("history")?;
    // `history`/`search` are `sessions.history.list`/`sessions.history.search`
    // registered under the `loom_session` server; this adapter exposes them
    // under its own legacy name, so route there explicitly rather than
    // through `SERVER_NAME` (which no operation projects onto).
    super::dispatch::call_tool(&client, "loom_session", name, arguments).await
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

    /// `history`/`search` route to `sessions.history.*` under the
    /// `loom_session` server; each tool name here must resolve there, since
    /// nothing is registered under this adapter's own `loom_history` name.
    #[test]
    fn history_tools_resolve_against_the_sessions_bundle() {
        for name in TOOL_NAMES {
            assert!(
                weaver_api::operation_for_mcp("loom_session", name).is_some(),
                "loom_session has no MCP projection for '{name}'"
            );
        }
    }
}
