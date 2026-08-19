//! Session lifecycle and normalized history, served by the generic registry
//! dispatcher.
//!
//! `get`, `summary`, `status_get`, `history`, and `search` are registered
//! `sessions.*` operations and route straight through
//! `super::dispatch::call_tool` — there is no `resolve_session_argument`
//! left to maintain for them; a caller-omitted session resolves to this
//! session through the same `#[operand(context)]` fill every other bundle
//! uses, in one `self_context()` call shared across the whole request rather
//! than one per tool.
//!
//! `status_set` (and its legacy alias `status`) stay hand-written: setting
//! the branch's status also posts a channel message, and the old adapter
//! reports that delivered message (`Client::get_channel(..).last_message`)
//! alongside the updated branch. `sessions.status.set`'s plain response is
//! only the branch, so routing this one through the generic dispatcher would
//! silently drop that delivery confirmation.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_session";
const TOOL_NAMES: [&str; 7] = [
    "get",
    "summary",
    "status_get",
    "status_set",
    "status",
    "history",
    "search",
];
const CANONICAL_TOOL_NAMES: [&str; 6] = [
    "get",
    "summary",
    "status_get",
    "status_set",
    "history",
    "search",
];
const READ_TOOLS: &[&str] = &["get", "summary", "status_get", "history", "search"];
const WRITE_TOOLS: &[&str] = &["status_set"];
const LEGACY_READ_TOOLS: &[&str] = &["get", "history", "search"];
const LEGACY_WRITE_TOOLS: &[&str] = &["status"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "loom/sessions/read@v1",
        group: "session",
        version: "v1",
        description: "Inspect visible sessions, catch-up state, status, and normalized history.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "loom/sessions/write@v1",
        group: "session",
        version: "v1",
        description: "Update this session's durable status projection and status stream.",
        tools: WRITE_TOOLS,
    },
    // Existing pinned sessions keep their exact identities. New profiles use
    // the canonical Loom sets.
    CapabilitySet {
        name: "mcp/session/read@v1",
        group: "session",
        version: "v1",
        description: "Inspect visible sessions and normalized session history.",
        tools: LEGACY_READ_TOOLS,
    },
    CapabilitySet {
        name: "mcp/session/status@v1",
        group: "session",
        version: "v1",
        description: "Update this session's durable status projection and status stream.",
        tools: LEGACY_WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "session",
    server_name: SERVER_NAME,
    description: "Session lifecycle, status projection, and normalized history.",
    capability_sets: || CAPABILITY_SETS,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn is_permission_rule(rule: &str) -> bool {
    super::is_builtin_permission_rule(SERVER_NAME, &TOOL_NAMES, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::expand_builtin_tool_set(SERVER_NAME, &TOOL_NAMES, CAPABILITY_SETS, name)
}

fn server_config() -> Value {
    super::builtin_server_config("session")
}

/// `status` is a legacy alias tool: it advertises the same schema as
/// `status_set` (the only operation registered under this server for
/// updating status) under its old name, for sessions pinned to
/// `mcp/session/status@v1` before the canonical rename.
fn tools() -> Value {
    let mut tools = weaver_api::mcp_tools_ordered(SERVER_NAME, &CANONICAL_TOOL_NAMES)
        .as_array()
        .expect("generated session MCP catalogue is an array")
        .clone();
    let mut legacy_status = tools
        .iter()
        .find(|tool| tool["name"] == "status_set")
        .expect("status_set is a registered session operation")
        .clone();
    legacy_status["name"] = json!("status");
    tools.insert(4, legacy_status);
    Value::Array(tools)
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown session tool '{name}'");
    }
    let client = super::runtime_client("session")?;
    match name {
        "status_set" | "status" => set_status(&client, name, arguments).await,
        // `status` and `status_set` are the same operation under two names;
        // every other tool here maps 1:1 onto its own registered operation.
        _ => super::dispatch::call_tool(&client, SERVER_NAME, name, arguments).await,
    }
}

/// Update the branch's durable status and report the channel message that
/// delivery produced — data `sessions.status.set`'s plain response (just the
/// updated branch) does not carry.
async fn set_status(client: &weaver_api::Client, name: &str, arguments: Value) -> Result<Value> {
    if !super::runtime_tool_allowed(name) {
        bail!("session tool '{name}' is not allowed by this session");
    }
    let level = super::string_argument(&arguments, "level")?.context("status requires level")?;
    if !matches!(level, "ok" | "attention" | "blocked") {
        bail!("level must be 'ok', 'attention', or 'blocked'");
    }
    let message = arguments
        .get("message")
        .and_then(Value::as_str)
        .context("status requires string message")?;
    if message.len() > 4096 {
        bail!("status message must be at most 4096 bytes");
    }
    let context = client.self_context().await?;
    let branch = client
        .set_branch_status(&context.branch_id, level, message)
        .await?;
    let channel = client.get_channel(&context.channel_id).await?;
    let value = json!({ "branch": branch, "status_message": channel.last_message });
    super::structured_result(&format!("status updated to {level}"), &value)
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tools_consolidate_status_and_history() {
        let surface = tools();
        let names = surface
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, TOOL_NAMES);
        assert_eq!(expand_tool_set("loom/sessions/read@v1").unwrap().len(), 5);
        assert_eq!(expand_tool_set("mcp/session/read@v1").unwrap().len(), 3);
        for name in CANONICAL_TOOL_NAMES {
            assert!(weaver_api::operation_for_mcp(SERVER_NAME, name).is_some());
        }
    }
}
