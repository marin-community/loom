//! Repository work items projected directly from Loom's operation registry.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use weaver_api::operations::issues as issue_operations;
use weaver_api::{CreateIssueReq, IssueActionsResult, IssueView, TagReq};

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

fn positive_id(arguments: &Value) -> Result<i64> {
    let id = arguments
        .get("id")
        .and_then(Value::as_i64)
        .context("id must be a positive integer")?;
    if id <= 0 {
        bail!("id must be a positive integer");
    }
    Ok(id)
}

async fn project_input(client: &weaver_api::Client, name: &str, arguments: Value) -> Result<Value> {
    match name {
        "list" => {
            let all = arguments
                .get("all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let context = client.self_context().await?;
            serde_json::to_value(issue_operations::ListInput {
                repo_root: context.repo_root,
                scope: issue_operations::ListScope::Repo,
                all,
            })
            .map_err(Into::into)
        }
        "add" => {
            let title = arguments
                .get("title")
                .and_then(Value::as_str)
                .context("title must be a non-empty string")?;
            if title.trim().is_empty() {
                bail!("title must be a non-empty string");
            }
            let body = arguments
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let context = client.self_context().await?;
            serde_json::to_value(issue_operations::CreateInput {
                branch: context.branch_id,
                request: CreateIssueReq {
                    title: title.to_string(),
                    body: body.to_string(),
                    ..Default::default()
                },
            })
            .map_err(Into::into)
        }
        "tag_set" => {
            let id = positive_id(&arguments)?;
            serde_json::to_value(issue_operations::SetTagInput {
                id,
                key: super::required_string_argument(&arguments, "key")?.to_string(),
                request: TagReq {
                    value: super::required_string_argument(&arguments, "value")?.to_string(),
                    note: arguments
                        .get("note")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    by: Some("agent".to_string()),
                },
            })
            .map_err(Into::into)
        }
        "tag_delete" => serde_json::to_value(issue_operations::DeleteTagInput {
            id: positive_id(&arguments)?,
            key: arguments
                .get("key")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .context("key must be a non-empty string")?
                .to_string(),
        })
        .map_err(Into::into),
        "get" | "close" | "reopen" | "delete" => {
            positive_id(&arguments)?;
            Ok(arguments)
        }
        _ => Ok(arguments),
    }
}

fn present(name: &str, input: &Value, output: Value) -> Result<Value> {
    match name {
        "list" => {
            let issues: Vec<IssueView> = serde_json::from_value(output)?;
            super::structured_result(&format!("{} work item(s)", issues.len()), &issues)
        }
        "get" | "add" => {
            let issue: IssueView = serde_json::from_value(output)?;
            let summary = if name == "add" {
                format!("created work item {}", issue.id)
            } else {
                format!("work item {}", issue.id)
            };
            super::structured_result(&summary, &issue)
        }
        "close" | "reopen" => {
            let issue: IssueView = serde_json::from_value(output)?;
            let result = IssueActionsResult {
                issues: vec![issue],
                deleted_ids: Vec::new(),
            };
            let id = input["id"].as_i64().unwrap_or_default();
            let past = if name == "close" {
                "closed"
            } else {
                "reopened"
            };
            super::structured_result(&format!("{past} work item {id}"), &result)
        }
        "delete" => {
            let id = input["id"].as_i64().unwrap_or_default();
            let result = IssueActionsResult {
                issues: Vec::new(),
                deleted_ids: vec![id],
            };
            super::structured_result(&format!("deleted work item {id}"), &result)
        }
        "tag_set" | "tag_delete" => {
            let issue: IssueView = serde_json::from_value(output)?;
            let id = issue.id;
            let result = IssueActionsResult {
                issues: vec![issue],
                deleted_ids: Vec::new(),
            };
            let action = if name == "tag_set" {
                "tagged"
            } else {
                "removed tag from"
            };
            super::structured_result(&format!("{action} work item {id}"), &result)
        }
        _ => super::structured_result("Loom operation complete", &output),
    }
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        // Resolution comes from the same descriptor used by tools/list.  The
        // match above only performs the few input/presentation projections;
        // it is not a second invocation registry.
        weaver_api::operation_for_mcp(SERVER_NAME, &name)
            .with_context(|| format!("unknown issue tool '{name}'"))?;
        let client = super::runtime_client("issue")?;
        let input = project_input(&client, &name, arguments).await?;
        let output =
            super::call_registered_tool("issue", SERVER_NAME, &name, input.clone()).await?;
        present(&name, &input, output)
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_surface_is_derived_from_operation_descriptors() {
        assert_eq!(tools().as_array().unwrap().len(), TOOL_NAMES.len());
        assert_eq!(expand_tool_set("loom/issues/read@v1").unwrap().len(), 2);
        assert_eq!(expand_tool_set("loom/issues/write@v1").unwrap().len(), 6);
        for name in TOOL_NAMES {
            assert!(weaver_api::operation_for_mcp(SERVER_NAME, name).is_some());
        }
    }

    #[test]
    fn positive_issue_id_is_accepted() {
        let arguments = serde_json::json!({"id": 7});
        assert_eq!(positive_id(&arguments).unwrap(), 7);
    }
}
