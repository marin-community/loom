//! Built-in MCP adapter for restricted GitHub sessions.
//!
//! All six tools call one registered operation,
//! `permissions.github.restricted.invoke`: the security boundary here is a
//! runtime check against the session's stored tool allowlist
//! (`session.policy_allowed_tools`), not a compile-time grant, so a restricted
//! session's policy — not its credential — decides which of these six tools
//! it may use. `super::dispatch::bind` assumes one tool name maps to one
//! operation; this is six tool names gated through one, so it keeps its own
//! schemas and call handler instead.
//!
//! Claude sees these fixed tools instead of `Bash`. The bridge carries only the
//! session-scoped Loom token and forwards each call to Loom's REST API; the
//! GitHub credential remains in Loom's profile/user-token store and never enters
//! the aggregate MCP process.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use weaver_api::operations::permissions::github::restricted::{self, invoke};

use super::{Adapter, CapabilitySet, ToolFuture};

const LOOM_COMMENT_TOOL_SET_V1: &str = "loom/github/comment@v1";
const GITHUB_TOOL_NAMES: [&str; 6] = [
    restricted::TOOLS[0].name,
    restricted::TOOLS[1].name,
    restricted::TOOLS[2].name,
    restricted::TOOLS[3].name,
    restricted::TOOLS[4].name,
    restricted::TOOLS[5].name,
];

/// Six tool names gated through one operation, so there is no name-to-operation
/// pair to export.
fn no_exports() -> &'static [super::dispatch::Export] {
    &[]
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "github",
    description: "Repository-scoped GitHub issue and pull-request operations.",
    capability_sets,
    exports: no_exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

const CAPABILITY_SETS: &[CapabilitySet] = &[CapabilitySet {
    name: LOOM_COMMENT_TOOL_SET_V1,
    group: "github",
    version: "v1",
    description: "Read, comment on, and edit the issue or pull request bound to the session.",
    tools: &GITHUB_TOOL_NAMES,
}];

fn capability_sets() -> &'static [CapabilitySet] {
    CAPABILITY_SETS
}

pub fn permission_rule(tool: &str) -> Option<String> {
    GITHUB_TOOL_NAMES
        .contains(&tool)
        .then(|| super::builtin_permission_rule("github", tool))
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    (name == LOOM_COMMENT_TOOL_SET_V1).then(|| {
        GITHUB_TOOL_NAMES
            .iter()
            .map(|tool| permission_rule(tool).expect("registered GitHub tool"))
            .collect()
    })
}

fn tools() -> Value {
    Value::Array(
        restricted::TOOLS
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.summary,
                    "inputSchema": (tool.schema)(),
                })
            })
            .collect(),
    )
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !GITHUB_TOOL_NAMES.contains(&name) {
        anyhow::bail!("unknown GitHub tool '{name}'");
    }
    if !super::runtime_adapter_tool_allowed("github", name) {
        anyhow::bail!("GitHub tool '{name}' is not allowed by this session");
    }
    let session_id =
        std::env::var("LOOM_SESSION_ID").context("restricted MCP is missing LOOM_SESSION_ID")?;
    let token = weaver_api::endpoint::token_from_env()
        .context("restricted MCP is missing its session-scoped LOOM_TOKEN")?;
    // The session and the tool are operands, not path segments, so neither is
    // percent-encoded: they travel in the JSON body.
    let view = weaver_api::Client::new(weaver_api::endpoint::base_url())
        .with_token(Some(token))
        .invoke::<invoke::Op>(&invoke::Input {
            tool: name.to_string(),
            arguments,
            session: session_id,
        })
        .await?;
    Ok(json!({
        "content": [{ "type": "text", "text": view.text }],
        "isError": false
    }))
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

#[cfg(test)]
mod tests {
    use super::{expand_tool_set, permission_rule, tools, GITHUB_TOOL_NAMES};

    #[test]
    fn surface_contains_only_fixed_github_operations() {
        let surface = tools();
        let names: Vec<&str> = surface
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, GITHUB_TOOL_NAMES);
    }

    #[test]
    fn comment_set_expands_to_the_fixed_surface() {
        let expanded = expand_tool_set("loom/github/comment@v1").unwrap();
        assert_eq!(expanded.len(), GITHUB_TOOL_NAMES.len());
        assert_eq!(expanded[0], permission_rule(GITHUB_TOOL_NAMES[0]).unwrap());
        assert!(expand_tool_set("mcp/github/admin").is_none());
    }
}
