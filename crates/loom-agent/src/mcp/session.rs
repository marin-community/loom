//! Session lifecycle and normalized history, served by the generic registry
//! dispatcher.
//!
//! `get`, `summary`, `status_get`, `history`, and `search` are registered
//! `sessions.*` operations and route straight through
//! `super::dispatch::call_tool`. A caller-omitted session resolves to this
//! session through the same `#[operand(context)]` fill every other bundle
//! uses, in one `self_context()` call shared across the whole request.
//!
//! `status_set` (and its legacy alias `status`) stay hand-written: setting
//! the branch's status also posts a channel message, and the old adapter
//! reports that delivered message (`Client::get_channel(..).last_message`)
//! alongside the updated branch. `sessions.status.set`'s plain response is
//! only the branch, so routing this one through the generic dispatcher would
//! silently drop that delivery confirmation.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use std::sync::OnceLock;

use weaver_api::operations::sessions;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_session";

/// The tools this server exports, in the order it advertises them.
///
/// `status` and `status_set` are the same operation under two names — the
/// former is what sessions pinned to `mcp/session/status@v1` called it before
/// the rename, and saying so here is the whole of that alias.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<sessions::get::Op>("get"),
            export::<sessions::summary::get::Op>("summary"),
            export::<sessions::status::get::Op>("status_get"),
            export::<sessions::status::set::Op>("status_set"),
            export::<sessions::status::set::Op>("status"),
            export::<sessions::history::list::Op>("history"),
            export::<sessions::history::search::Op>("search"),
        ]
    })
}
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
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, CAPABILITY_SETS, name)
}

fn server_config() -> Value {
    super::builtin_server_config("session")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if super::dispatch::lookup(exports(), name).is_none() {
        bail!("unknown session tool '{name}'");
    }
    let client = super::runtime_client("session")?;
    match name {
        "status_set" | "status" => set_status(&client, name, arguments).await,
        // `status` and `status_set` are the same operation under two names;
        // every other tool here maps 1:1 onto its own registered operation.
        _ => super::dispatch::call_tool(&client, SERVER_NAME, exports(), name, arguments).await,
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
mod tests {}
