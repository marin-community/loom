//! Session lifecycle and normalized history, served by the generic registry
//! dispatcher.
//!
//! `get`, `summary`, `status_get`, `history`, and `search` route straight
//! through `super::dispatch::call_tool`; a caller-omitted session resolves via
//! the same `#[operand(context)]` fill every other bundle uses.
//!
//! `status_set` stays hand-written: setting the
//! branch's status also posts a channel message, and callers expect that
//! message back alongside the updated branch — `sessions.status.set`'s own
//! response carries only the branch.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use std::sync::OnceLock;

use weaver_api::operations::sessions;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};
use weaver_api::operations::{branches, channels};

/// The tools this server exports, in the order it advertises them.
///
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<sessions::get::Op>("get"),
            export::<sessions::summary::get::Op>("summary"),
            export::<sessions::status::get::Op>("status_get"),
            export::<sessions::status::set::Op>("status_set"),
            export::<sessions::history::list::Op>("history"),
            export::<sessions::history::search::Op>("search"),
        ]
    })
}
const READ_TOOLS: &[&str] = &["get", "summary", "status_get", "history", "search"];
const WRITE_TOOLS: &[&str] = &["status_set"];
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
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "session",
    description: "Session lifecycle, status projection, and normalized history.",
    capability_sets: || CAPABILITY_SETS,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("session", CAPABILITY_SETS, name)
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
    if !super::runtime_adapter_tool_allowed("session", name) {
        bail!("session tool '{name}' is not allowed by this session");
    }
    let client = super::runtime_client("session")?;
    match name {
        "status_set" => set_status(&client, arguments).await,
        _ => super::dispatch::call_tool(&client, "session", exports(), name, arguments).await,
    }
}

/// Applies the status update, then folds in the channel message it posted
/// (which `sessions.status.set`'s own response omits).
async fn set_status(client: &weaver_api::Client, arguments: Value) -> Result<Value> {
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
    let context = client
        .invoke::<sessions::context::Op>(&sessions::context::Input {
            session: String::new(),
        })
        .await?;
    let branch = client
        .invoke::<branches::status::set::Op>(&branches::status::set::Input {
            level: level.to_string(),
            message: (!message.is_empty()).then(|| message.to_string()),
            branch: context.branch_id.to_string(),
        })
        .await?;
    let channel = client
        .invoke::<channels::get::Op>(&channels::get::Input {
            channel: context.channel_id.to_string(),
            branch: String::new(),
        })
        .await?;
    let value = json!({ "branch": branch, "status_message": channel.last_message });
    super::structured_result(&format!("status updated to {level}"), &value)
}

#[cfg(test)]
mod tests {}
