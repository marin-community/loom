//! Repository-scoped work items projected from Loom's REST API.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use weaver_api::{CreateIssueReq, IssueAction, IssueActionsReq};

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_issue";
const TOOL_NAMES: [&str; 8] = [
    "list",
    "get",
    "add",
    "close",
    "reopen",
    "delete",
    "tag_set",
    "tag_delete",
];
const READ_TOOLS: &[&str] = &["list", "get"];
const WRITE_TOOLS: &[&str] = &["add", "close", "reopen", "delete", "tag_set", "tag_delete"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "loom/issues/read@v1",
        group: "issue",
        version: "v1",
        description: "List and inspect work items in this session's repository.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "loom/issues/write@v1",
        group: "issue",
        version: "v1",
        description: "Create, update, tag, and remove repository work items.",
        tools: WRITE_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "issue",
    server_name: SERVER_NAME,
    description: "Repository work items, lifecycle, and free-form tags.",
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
    super::builtin_server_config("issue")
}

fn tools() -> Value {
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

fn integer_argument(arguments: &Value, key: &str) -> Result<i64> {
    arguments
        .get(key)
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .with_context(|| format!("{key} must be a positive integer"))
}

async fn assert_repository_issue(
    client: &weaver_api::Client,
    id: i64,
    repo_root: &str,
) -> Result<weaver_api::IssueView> {
    let issue = client.get_issue(id).await?;
    if issue.repo_root != repo_root {
        bail!("work item {id} is outside this session's repository");
    }
    Ok(issue)
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown issue tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("issue tool '{name}' is not allowed by this session");
    }
    arguments
        .as_object()
        .context("issue tool arguments must be an object")?;
    let client = super::runtime_client("issue")?;
    let context = client.self_context().await?;
    match name {
        "list" => {
            let all = arguments
                .get("all")
                .map(|value| value.as_bool().context("all must be a boolean"))
                .transpose()?
                .unwrap_or(false);
            let issues = client
                .list_repo_issues(&context.repo_root, "repo", all)
                .await?;
            super::structured_result(&format!("{} work item(s)", issues.len()), &issues)
        }
        "get" => {
            let id = integer_argument(&arguments, "id")?;
            let issue = assert_repository_issue(&client, id, &context.repo_root).await?;
            super::structured_result(&format!("work item {id}"), &issue)
        }
        "add" => {
            let title =
                super::string_argument(&arguments, "title")?.context("add requires title")?;
            let body = super::string_argument(&arguments, "body")?.unwrap_or_default();
            let issue = client
                .create_branch_issue(
                    &context.branch_id,
                    &CreateIssueReq {
                        title: title.to_string(),
                        body: body.to_string(),
                        ..Default::default()
                    },
                )
                .await?;
            super::structured_result(&format!("created work item {}", issue.id), &issue)
        }
        "close" | "reopen" => {
            let id = integer_argument(&arguments, "id")?;
            assert_repository_issue(&client, id, &context.repo_root).await?;
            let result = client
                .issue_actions(&IssueActionsReq {
                    ids: vec![id],
                    action: if name == "close" {
                        IssueAction::Close
                    } else {
                        IssueAction::Reopen
                    },
                })
                .await?;
            super::structured_result(&format!("{} work item {id}", name), &result)
        }
        "delete" => {
            let id = integer_argument(&arguments, "id")?;
            assert_repository_issue(&client, id, &context.repo_root).await?;
            let result = client
                .issue_actions(&IssueActionsReq {
                    ids: vec![id],
                    action: IssueAction::Delete,
                })
                .await?;
            super::structured_result(&format!("deleted work item {id}"), &result)
        }
        "tag_set" => {
            let id = integer_argument(&arguments, "id")?;
            assert_repository_issue(&client, id, &context.repo_root).await?;
            let key = super::string_argument(&arguments, "key")?.context("tag_set requires key")?;
            let value =
                super::string_argument(&arguments, "value")?.context("tag_set requires value")?;
            let note = super::string_argument(&arguments, "note")?.unwrap_or_default();
            let result = client
                .issue_actions(&IssueActionsReq {
                    ids: vec![id],
                    action: IssueAction::Tag {
                        key: key.to_string(),
                        value: value.to_string(),
                        note: note.to_string(),
                        by: Some("agent".to_string()),
                    },
                })
                .await?;
            super::structured_result(&format!("tagged work item {id}"), &result)
        }
        "tag_delete" => {
            let id = integer_argument(&arguments, "id")?;
            assert_repository_issue(&client, id, &context.repo_root).await?;
            let key =
                super::string_argument(&arguments, "key")?.context("tag_delete requires key")?;
            let result = client
                .issue_actions(&IssueActionsReq {
                    ids: vec![id],
                    action: IssueAction::Untag {
                        key: key.to_string(),
                    },
                })
                .await?;
            super::structured_result(&format!("removed tag from work item {id}"), &result)
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
    fn issue_surface_is_registered_by_risk() {
        assert_eq!(tools().as_array().unwrap().len(), TOOL_NAMES.len());
        assert_eq!(expand_tool_set("loom/issues/read@v1").unwrap().len(), 2);
        assert_eq!(expand_tool_set("loom/issues/write@v1").unwrap().len(), 6);
    }
}
