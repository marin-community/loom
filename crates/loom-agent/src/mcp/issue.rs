//! Repository-scoped work items projected from typed Loom API contracts.

use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::Value;
use weaver_api::operations::issues as issue_operations;
use weaver_api::{CreateIssueReq, IssueActionsResult, TagReq};

use super::{
    Adapter, CapabilitySet, ProjectionFuture, RemoteProjection, RemoteToolBinding, ServeFuture,
    ToolFuture,
};

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
    super::validate_remote_tool_bindings(SERVER_NAME, &TOOL_NAMES, REMOTE_TOOLS);
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

#[derive(Debug, Deserialize)]
struct ListArgs {
    #[serde(default)]
    all: bool,
}

#[derive(Debug, Deserialize)]
struct IdArgs {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct AddArgs {
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(Debug, Deserialize)]
struct SetTagArgs {
    id: i64,
    key: String,
    value: String,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize)]
struct DeleteTagArgs {
    id: i64,
    key: String,
}

fn positive_id(id: i64) -> Result<i64> {
    if id <= 0 {
        bail!("id must be a positive integer");
    }
    Ok(id)
}

struct ListProjection;

impl RemoteProjection for ListProjection {
    type Operation = issue_operations::List;
    type Args = ListArgs;

    const ADAPTER: &'static str = "issue";
    const TOOL: &'static str = "list";

    fn project(
        client: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<issue_operations::ListInput> {
        Box::pin(async move {
            let context = client.self_context().await?;
            Ok(issue_operations::ListInput {
                repo_root: context.repo_root,
                scope: issue_operations::ListScope::Repo,
                all: args.all,
            })
        })
    }

    fn present(
        _: &issue_operations::ListInput,
        issues: Vec<weaver_api::IssueView>,
    ) -> Result<Value> {
        super::structured_result(&format!("{} work item(s)", issues.len()), &issues)
    }
}

struct GetProjection;

impl RemoteProjection for GetProjection {
    type Operation = issue_operations::Get;
    type Args = IdArgs;

    const ADAPTER: &'static str = "issue";
    const TOOL: &'static str = "get";

    fn project(
        _: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<issue_operations::IdInput> {
        Box::pin(async move {
            Ok(issue_operations::IdInput {
                id: positive_id(args.id)?,
            })
        })
    }

    fn present(input: &issue_operations::IdInput, issue: weaver_api::IssueView) -> Result<Value> {
        super::structured_result(&format!("work item {}", input.id), &issue)
    }
}

struct AddProjection;

impl RemoteProjection for AddProjection {
    type Operation = issue_operations::Create;
    type Args = AddArgs;

    const ADAPTER: &'static str = "issue";
    const TOOL: &'static str = "add";

    fn project(
        client: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<issue_operations::CreateInput> {
        Box::pin(async move {
            let context = client.self_context().await?;
            Ok(issue_operations::CreateInput {
                branch: context.branch_id,
                request: CreateIssueReq {
                    title: args.title,
                    body: args.body,
                    ..Default::default()
                },
            })
        })
    }

    fn present(_: &issue_operations::CreateInput, issue: weaver_api::IssueView) -> Result<Value> {
        super::structured_result(&format!("created work item {}", issue.id), &issue)
    }
}

macro_rules! status_projection {
    ($projection:ident, $operation:ty, $tool:literal, $past:literal) => {
        struct $projection;

        impl RemoteProjection for $projection {
            type Operation = $operation;
            type Args = IdArgs;

            const ADAPTER: &'static str = "issue";
            const TOOL: &'static str = $tool;

            fn project(
                _: weaver_api::Client,
                args: Self::Args,
            ) -> ProjectionFuture<issue_operations::IdInput> {
                Box::pin(async move {
                    Ok(issue_operations::IdInput {
                        id: positive_id(args.id)?,
                    })
                })
            }

            fn present(
                input: &issue_operations::IdInput,
                issue: weaver_api::IssueView,
            ) -> Result<Value> {
                let result = IssueActionsResult {
                    issues: vec![issue],
                    deleted_ids: Vec::new(),
                };
                super::structured_result(&format!("{} work item {}", $past, input.id), &result)
            }
        }
    };
}

status_projection!(CloseProjection, issue_operations::Close, "close", "close");
status_projection!(
    ReopenProjection,
    issue_operations::Reopen,
    "reopen",
    "reopen"
);

struct DeleteProjection;

impl RemoteProjection for DeleteProjection {
    type Operation = issue_operations::Delete;
    type Args = IdArgs;

    const ADAPTER: &'static str = "issue";
    const TOOL: &'static str = "delete";

    fn project(
        _: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<issue_operations::IdInput> {
        Box::pin(async move {
            Ok(issue_operations::IdInput {
                id: positive_id(args.id)?,
            })
        })
    }

    fn present(
        input: &issue_operations::IdInput,
        _: weaver_api::DeleteIssueResult,
    ) -> Result<Value> {
        let result = IssueActionsResult {
            issues: Vec::new(),
            deleted_ids: vec![input.id],
        };
        super::structured_result(&format!("deleted work item {}", input.id), &result)
    }
}

struct SetTagProjection;

impl RemoteProjection for SetTagProjection {
    type Operation = issue_operations::SetTag;
    type Args = SetTagArgs;

    const ADAPTER: &'static str = "issue";
    const TOOL: &'static str = "tag_set";

    fn project(
        _: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<issue_operations::SetTagInput> {
        Box::pin(async move {
            Ok(issue_operations::SetTagInput {
                id: positive_id(args.id)?,
                key: args.key,
                request: TagReq {
                    value: args.value,
                    note: args.note,
                    by: Some("agent".to_string()),
                },
            })
        })
    }

    fn present(
        input: &issue_operations::SetTagInput,
        issue: weaver_api::IssueView,
    ) -> Result<Value> {
        let result = IssueActionsResult {
            issues: vec![issue],
            deleted_ids: Vec::new(),
        };
        super::structured_result(&format!("tagged work item {}", input.id), &result)
    }
}

struct DeleteTagProjection;

impl RemoteProjection for DeleteTagProjection {
    type Operation = issue_operations::DeleteTag;
    type Args = DeleteTagArgs;

    const ADAPTER: &'static str = "issue";
    const TOOL: &'static str = "tag_delete";

    fn project(
        _: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<issue_operations::DeleteTagInput> {
        Box::pin(async move {
            Ok(issue_operations::DeleteTagInput {
                id: positive_id(args.id)?,
                key: args.key,
            })
        })
    }

    fn present(
        input: &issue_operations::DeleteTagInput,
        issue: weaver_api::IssueView,
    ) -> Result<Value> {
        let result = IssueActionsResult {
            issues: vec![issue],
            deleted_ids: Vec::new(),
        };
        super::structured_result(&format!("removed tag from work item {}", input.id), &result)
    }
}

const REMOTE_TOOLS: &[RemoteToolBinding] = &[
    RemoteToolBinding::new::<ListProjection>(),
    RemoteToolBinding::new::<GetProjection>(),
    RemoteToolBinding::new::<AddProjection>(),
    RemoteToolBinding::new::<CloseProjection>(),
    RemoteToolBinding::new::<ReopenProjection>(),
    RemoteToolBinding::new::<DeleteProjection>(),
    RemoteToolBinding::new::<SetTagProjection>(),
    RemoteToolBinding::new::<DeleteTagProjection>(),
];

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { super::call_remote_tool("issue", REMOTE_TOOLS, &name, arguments).await })
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
        assert_eq!(REMOTE_TOOLS.len(), TOOL_NAMES.len());
        assert_eq!(expand_tool_set("loom/issues/read@v1").unwrap().len(), 2);
        assert_eq!(expand_tool_set("loom/issues/write@v1").unwrap().len(), 6);
    }
}
