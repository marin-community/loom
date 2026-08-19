//! Versioned artifact operations projected from Loom's REST API.
//!
//! TODO(registry): not yet ported — `artifacts.*` has no operation registry
//! bundle yet, so this adapter keeps its own hand-written schemas, argument
//! projection, and capability sets rather than `super::dispatch::bind`.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use weaver_api::{AnchorDto, ArtifactUpsertReq};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_artifact";
const TOOL_NAMES: [&str; 8] = [
    "list", "get", "write", "delete", "history", "threads", "comment", "resolve",
];
const READ_TOOLS: &[&str] = &["list", "get", "history", "threads"];
const WRITE_TOOLS: &[&str] = &["write", "delete", "comment", "resolve"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "loom/artifacts/read@v1",
        group: "artifact",
        version: "v1",
        description: "List and read versioned artifacts and their discussions.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "loom/artifacts/write@v1",
        group: "artifact",
        version: "v1",
        description: "Write, delete, comment on, and resolve versioned artifacts.",
        tools: WRITE_TOOLS,
    },
    CapabilitySet {
        name: "mcp/artifact/read@v1",
        group: "artifact",
        version: "v1",
        description: "List and read versioned artifacts and their discussions.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "mcp/artifact/write@v1",
        group: "artifact",
        version: "v1",
        description: "Write, delete, comment on, and resolve versioned artifacts.",
        tools: WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "artifact",
    server_name: SERVER_NAME,
    description: "Named, versioned deliverables and anchored review threads.",
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
        "delete" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("delete requires name")?;
            let repo = repo_scope(&arguments)?;
            let artifact = client
                .get_branch_artifact(&branch, artifact_name, None, repo)
                .await?;
            client
                .delete_branch_artifact(&branch, artifact_name, repo)
                .await?;
            super::structured_result(
                &format!("deleted artifact {artifact_name}"),
                &json!({ "deleted": true, "artifact": artifact.meta }),
            )
        }
        "history" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("history requires name")?;
            let artifact = client
                .get_branch_artifact(&branch, artifact_name, None, repo_scope(&arguments)?)
                .await?;
            super::structured_result(
                &format!(
                    "{} revision(s) for {artifact_name}",
                    artifact.versions.len()
                ),
                &artifact.versions,
            )
        }
        "threads" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("threads requires name")?;
            let all = arguments
                .get("all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut threads = client.list_branch_threads(&branch, artifact_name).await?;
            if !all {
                threads.retain(|thread| thread.status == "open");
            }
            super::structured_result(
                &format!("{} review thread(s) on {artifact_name}", threads.len()),
                &threads,
            )
        }
        "comment" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("comment requires name")?;
            let body =
                super::string_argument(&arguments, "body")?.context("comment requires body")?;
            if let Some(thread_id) = arguments.get("thread_id").and_then(Value::as_i64) {
                if thread_id <= 0 {
                    bail!("thread_id must be positive");
                }
                let comment = client
                    .add_branch_thread_comment(&branch, artifact_name, thread_id, body)
                    .await?;
                super::structured_result(&format!("commented on thread {thread_id}"), &comment)
            } else {
                let quote = super::string_argument(&arguments, "quote")?
                    .context("comment requires quote when thread_id is omitted")?;
                let base_rev = match arguments.get("base_rev") {
                    Some(value) => value.as_i64().context("base_rev must be an integer")?,
                    None => {
                        client
                            .get_branch_artifact(&branch, artifact_name, None, false)
                            .await?
                            .meta
                            .rev
                    }
                };
                if base_rev <= 0 {
                    bail!("base_rev must be positive");
                }
                let thread = client
                    .create_branch_thread(
                        &branch,
                        artifact_name,
                        base_rev,
                        AnchorDto {
                            quote: quote.to_string(),
                            prefix: arguments
                                .get("prefix")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            suffix: arguments
                                .get("suffix")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        },
                        body,
                    )
                    .await?;
                super::structured_result(&format!("opened review thread {}", thread.id), &thread)
            }
        }
        "resolve" => {
            let artifact_name =
                super::string_argument(&arguments, "name")?.context("resolve requires name")?;
            let thread_id = arguments
                .get("thread_id")
                .and_then(Value::as_i64)
                .filter(|value| *value > 0)
                .context("resolve requires a positive thread_id")?;
            let value = client
                .resolve_branch_thread(&branch, artifact_name, thread_id)
                .await?;
            super::structured_result(&format!("resolved thread {thread_id}"), &value)
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
    }
}
