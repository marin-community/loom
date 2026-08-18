//! Session lifecycle and normalized history projected from Loom's REST API.

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
    // Existing pinned sessions keep their exact identities during the CLI/MCP
    // migration. New profile resolution receives the canonical Loom bundles.
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

fn history_args(arguments: &Value) -> Result<(Option<&str>, Option<usize>, Vec<String>)> {
    let before = super::string_argument(arguments, "before")?;
    let limit = arguments
        .get("limit")
        .map(|value| {
            let limit = value.as_u64().context("limit must be a positive integer")?;
            let limit = usize::try_from(limit).context("limit is too large")?;
            (limit > 0 && limit <= crate::history::MAX_LIMIT)
                .then_some(limit)
                .context("limit is outside the supported range")
        })
        .transpose()?;
    let kinds = arguments
        .get("kinds")
        .map(|value| {
            value
                .as_array()
                .context("kinds must be an array")?
                .iter()
                .map(|kind| {
                    kind.as_str()
                        .context("every kind must be a string")
                        .map(str::to_string)
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok((before, limit, kinds))
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown session tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("session tool '{name}' is not allowed by this session");
    }
    arguments
        .as_object()
        .context("session tool arguments must be an object")?;
    let client = super::runtime_client("session")?;
    match name {
        "get" => {
            let id = super::resolve_session_argument(&client, &arguments).await?;
            let session = client.get_session(&id).await?;
            super::structured_result(&format!("session {id}"), &session)
        }
        "summary" => {
            let id = super::resolve_session_argument(&client, &arguments).await?;
            let summary = client.session_summary(&id).await?;
            super::structured_result(&format!("session {id} summary"), &summary)
        }
        "status_get" => {
            let id = super::resolve_session_argument(&client, &arguments).await?;
            let session = client.get_session(&id).await?;
            let attention = session
                .branch
                .tags
                .iter()
                .find(|tag| tag.key == weaver_core::tags::ATTENTION_KEY)
                .map(|tag| tag.value.as_str())
                .unwrap_or("ok");
            let value = json!({
                "session_id": session.id,
                "attention": attention,
                "message": session.branch.description,
            });
            super::structured_result(&format!("session {id} status"), &value)
        }
        "status_set" | "status" => {
            let level =
                super::string_argument(&arguments, "level")?.context("status requires level")?;
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
        "history" | "search" => {
            let id = super::resolve_session_argument(&client, &arguments).await?;
            let (before, limit, kinds) = history_args(&arguments)?;
            let page = if name == "history" {
                client
                    .get_session_history(&id, before, limit, &kinds)
                    .await?
            } else {
                let query =
                    super::string_argument(&arguments, "q")?.context("search requires q")?;
                client
                    .search_session_history(&id, query, before, limit, &kinds)
                    .await?
            };
            super::structured_result(
                &format!("{} normalized history record(s)", page.records.len()),
                &page,
            )
        }
        _ => unreachable!(),
    }
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
    }
}
