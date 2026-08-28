//! Versioned artifact operations, served by the generic registry dispatcher.
//!
//! Every tool here is a registered `artifacts.*` operation, and `tools/list` is
//! read off [`exports`]. `delete`, `history`, `threads`, `comment`, and
//! `resolve` route straight through `super::dispatch::call_tool`. The
//! generated schema is all these tools need.
//!
//! `list`, `get`, and `write` are hand-written because they resolve each artifact's
//! dashboard `url` with a second REST call (`Client::branch_artifact_url`) and merge
//! it into the response. The generic operation response lacks this `url` field.

use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use weaver_api::operations::artifacts;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};
use weaver_api::operations::sessions;

/// The tools this server exports, in the order it advertises them. `list`,
/// `get`, and `write` are served by hand below — they resolve each artifact's
/// dashboard `url` with a second call — but they are the same operations, so
/// the catalogue and the capability sets still come from here.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<artifacts::list::Op>("list"),
            export::<artifacts::get::Op>("get"),
            export::<artifacts::write::Op>("write"),
            export::<artifacts::delete::Op>("delete"),
            export::<artifacts::history::Op>("history"),
            export::<artifacts::threads::list::Op>("threads"),
            export::<artifacts::threads::comment::Op>("comment"),
            export::<artifacts::threads::resolve::Op>("resolve"),
        ]
    })
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "artifact",
    description: "Named, versioned deliverables and anchored review threads.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

/// Capability sets are derived from the registry: every `artifacts.*` operation
/// exposed as an MCP tool on this server contributes its tool to the set
/// named by its grant.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        super::dispatch::capability_sets(exports(), "artifact", describe_capability)
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/artifacts/read@v1" => "List and read versioned artifacts and their discussions.",
        "loom/artifacts/write@v1" => "Write, delete, comment on, and resolve versioned artifacts.",
        _ => "Versioned artifact operations.",
    }
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("artifact", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
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
    if super::dispatch::lookup(exports(), name).is_none() {
        bail!("unknown artifact tool '{name}'");
    }
    if !super::runtime_adapter_tool_allowed("artifact", name) {
        bail!("artifact tool '{name}' is not allowed by this session");
    }
    arguments
        .as_object()
        .context("artifact tool arguments must be an object")?;
    let client = super::runtime_client("artifact")?;
    match name {
        "list" | "get" | "write" => with_dashboard_url(&client, name, arguments).await,
        _ => super::dispatch::call_tool(&client, "artifact", exports(), name, arguments).await,
    }
}

/// `list`/`get`/`write` merge in a dashboard `url` the plain operation
/// response does not carry, so they stay outside the generic dispatcher.
async fn with_dashboard_url(
    client: &weaver_api::Client,
    name: &str,
    arguments: Value,
) -> Result<Value> {
    let context = client
        .invoke::<sessions::context::Op>(&sessions::context::Input {
            session: String::new(),
        })
        .await?;
    let branch = context.branch_id;
    match name {
        "list" => {
            let repo = repo_scope(&arguments)?;
            let mut artifacts = client
                .invoke::<artifacts::list::Op>(&artifacts::list::Input {
                    repo,
                    branch: branch.to_string(),
                })
                .await?;
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
                .invoke::<artifacts::get::Op>(&artifacts::get::Input {
                    name: artifact_name.to_string(),
                    rev,
                    repo: (repo_scope(&arguments)?),
                    branch: branch.to_string(),
                })
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
                .invoke::<artifacts::write::Op>(&artifacts::write::Input {
                    name: artifact_name.to_string(),
                    content: content.to_string(),
                    title: (arguments
                        .get("title")
                        .map(|value| {
                            value
                                .as_str()
                                .context("title must be a string")
                                .map(str::to_string)
                        })
                        .transpose()?)
                    .clone(),
                    kind: (super::string_argument(&arguments, "kind")?.map(str::to_string)).clone(),
                    base_rev,
                    repo: (repo_scope(&arguments)?),
                    branch: branch.to_string(),
                })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_sets_are_grouped_by_grant() {
        assert_eq!(expand_tool_set("loom/artifacts/read@v1").unwrap().len(), 4);
        assert_eq!(expand_tool_set("loom/artifacts/write@v1").unwrap().len(), 4);
        assert!(expand_tool_set("mcp/artifact/read@v1").is_none());
    }
}
