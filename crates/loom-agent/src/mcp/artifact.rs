//! Versioned artifact operations, served by the generic registry dispatcher.
//!
//! Every tool here is a registered `artifacts.*` operation, and `tools/list`
//! is `weaver_api::mcp_tools_ordered(SERVER_NAME, TOOL_NAMES)`. `delete`,
//! `history`, `threads`, `comment`, and `resolve` route straight through
//! `super::dispatch::call_tool` — there is no `project_input` left for them to
//! maintain (`threads`'s `open_only` flag and `comment`'s tagged
//! `New`/`Reply` target are exactly what the schema already advertised; the
//! old hand code here still read a stale `all` flag and flat
//! `thread_id`/`quote` arguments that predated it).
//!
//! `list`, `get`, and `write` stay hand-written for one reason: they resolve
//! each artifact's dashboard `url` with a second REST call
//! (`Client::branch_artifact_url`) and merge it into the response.
//! `ArtifactMeta`/`ArtifactView` carry no `url` field the way
//! `ChannelView::bindings` now carries channel bindings, so routing these
//! three through the plain operation would silently drop that link.

use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use weaver_api::ArtifactUpsertReq;

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_artifact";
const TOOL_NAMES: [&str; 8] = [
    "list", "get", "write", "delete", "history", "threads", "comment", "resolve",
];

// Existing pinned sessions keep their exact identities during the CLI/MCP
// migration: `mcp/artifact/*@v1` predates the `loom/artifacts/*@v1` grant
// names and has no registry counterpart to derive from.
const LEGACY_READ_TOOLS: &[&str] = &["list", "get", "history", "threads"];
const LEGACY_WRITE_TOOLS: &[&str] = &["write", "delete", "comment", "resolve"];
const LEGACY_CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "mcp/artifact/read@v1",
        group: "artifact",
        version: "v1",
        description: "List and read versioned artifacts and their discussions.",
        tools: LEGACY_READ_TOOLS,
    },
    CapabilitySet {
        name: "mcp/artifact/write@v1",
        group: "artifact",
        version: "v1",
        description: "Write, delete, comment on, and resolve versioned artifacts.",
        tools: LEGACY_WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "artifact",
    server_name: SERVER_NAME,
    description: "Named, versioned deliverables and anchored review threads.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

/// Capability sets, derived from the registry rather than hand-maintained:
/// every `artifacts.*` operation whose MCP projection targets this server
/// contributes its tool to the set named by its grant.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets =
            super::dispatch::derive_capability_sets(SERVER_NAME, "artifact", describe_capability);
        sets.extend(LEGACY_CAPABILITY_SETS.iter().map(|set| CapabilitySet {
            name: set.name,
            group: set.group,
            version: set.version,
            description: set.description,
            tools: set.tools,
        }));
        sets
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/artifacts/read@v1" => "List and read versioned artifacts and their discussions.",
        "loom/artifacts/write@v1" => "Write, delete, comment on, and resolve versioned artifacts.",
        _ => "Versioned artifact operations.",
    }
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    capability_sets()
        .iter()
        .find(|set| set.name == name)
        .map(|set| {
            set.tools
                .iter()
                .map(|tool| format!("mcp__{SERVER_NAME}__{tool}"))
                .collect()
        })
}

fn server_config() -> Value {
    super::builtin_server_config("artifact")
}

fn tools() -> Value {
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

fn repo_scope(arguments: &Value) -> Result<bool> {
    match super::string_argument(arguments, "scope")?.unwrap_or("branch") {
        "branch" => Ok(false),
        "repo" => Ok(true),
        value => bail!("scope must be 'branch' or 'repo', got '{value}'"),
    }
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown artifact tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("artifact tool '{name}' is not allowed by this session");
    }
    arguments
        .as_object()
        .context("artifact tool arguments must be an object")?;
    let client = super::runtime_client("artifact")?;
    match name {
        "list" | "get" | "write" => with_dashboard_url(&client, name, arguments).await,
        _ => super::dispatch::call_tool(&client, SERVER_NAME, name, arguments).await,
    }
}

/// `list`/`get`/`write` merge in a dashboard `url` the plain operation
/// response does not carry, so they stay outside the generic dispatcher.
async fn with_dashboard_url(
    client: &weaver_api::Client,
    name: &str,
    arguments: Value,
) -> Result<Value> {
    let context = client.self_context().await?;
    let branch = context.branch_id;
    match name {
        "list" => {
            let repo = repo_scope(&arguments)?;
            let mut artifacts = client.list_branch_artifacts(&branch, repo).await?;
            if repo {
                artifacts.retain(|artifact| artifact.branch_id.is_none());
            }
            let mut items = Vec::with_capacity(artifacts.len());
            for artifact in artifacts {
                let url = client.branch_artifact_url(&branch, &artifact.name).await?;
                items.push(json!({ "artifact": artifact, "url": url }));
            }
            super::structured_result(&format!("{} artifact(s)", items.len()), &items)
        }
        "get" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("get requires name")?;
            let rev = arguments
                .get("rev")
                .map(|value| {
                    let rev = value.as_i64().context("rev must be an integer")?;
                    (rev > 0).then_some(rev).context("rev must be positive")
                })
                .transpose()?;
            let artifact = client
                .get_branch_artifact(&branch, artifact_name, rev, repo_scope(&arguments)?)
                .await?;
            let url = client.branch_artifact_url(&branch, artifact_name).await?;
            let value = json!({ "artifact": artifact, "url": url });
            super::structured_result(&format!("artifact {artifact_name}"), &value)
        }
        "write" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("write requires name")?;
            let content = arguments
                .get("content")
                .and_then(Value::as_str)
                .context("write requires string content")?;
            let base_rev = arguments
                .get("base_rev")
                .map(|value| {
                    let rev = value.as_i64().context("base_rev must be an integer")?;
                    (rev >= 0)
                        .then_some(rev)
                        .context("base_rev must be non-negative")
                })
                .transpose()?;
            let artifact = client
                .write_branch_artifact(
                    &branch,
                    artifact_name,
                    &ArtifactUpsertReq {
                        content: content.to_string(),
                        title: arguments
                            .get("title")
                            .map(|value| {
                                value
                                    .as_str()
                                    .context("title must be a string")
                                    .map(str::to_string)
                            })
                            .transpose()?,
                        kind: super::string_argument(&arguments, "kind")?.map(str::to_string),
                        author: None,
                        repo: repo_scope(&arguments)?,
                        base_rev,
                    },
                )
                .await?;
            let revision = artifact.meta.rev;
            let url = client.branch_artifact_url(&branch, artifact_name).await?;
            let value = json!({ "artifact": artifact, "url": url });
            super::structured_result(
                &format!("wrote artifact {artifact_name} revision {revision}"),
                &value,
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
    fn artifact_tools_use_resource_verbs() {
        let surface = tools();
        let names = surface
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, TOOL_NAMES);
        assert!(surface
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "write")
            .unwrap()["inputSchema"]["properties"]
            .get("base_rev")
            .is_some());
        for name in &names {
            assert!(weaver_api::operation_for_mcp(SERVER_NAME, name).is_some());
        }
    }

    #[test]
    fn capability_sets_are_grouped_by_grant() {
        assert_eq!(expand_tool_set("loom/artifacts/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("loom/artifacts/write@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("mcp/artifact/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("mcp/artifact/write@v1").unwrap().len(), 4);
    }
}
