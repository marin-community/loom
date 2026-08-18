//! Typed API contracts and user-facing projections for repository work items.

use serde::{Deserialize, Serialize};

use crate::{
    CreateIssueReq, CreateRepoIssueReq, DeleteIssueResult, IssueActionsReq, IssueActionsResult,
    IssueView, TagReq,
};

use super::*;

const READ: &[&str] = &["loom/issues/read@v1"];
const WRITE: &[&str] = &["loom/issues/write@v1"];

const fn id_arg() -> ArgumentSpec {
    ArgumentSpec::integer("id")
        .minimum(1)
        .description("A Loom work-item id.")
        .required()
}

static LIST_ARGS: &[ArgumentSpec] = &[ArgumentSpec::boolean("all").default_boolean(false)];
static ID_ARGS: &[ArgumentSpec] = &[id_arg()];
static ADD_ARGS: &[ArgumentSpec] = &[
    ArgumentSpec::string("title").minimum(1).required(),
    ArgumentSpec::string("body"),
];
static TAG_SET_ARGS: &[ArgumentSpec] = &[
    id_arg(),
    ArgumentSpec::string("key").minimum(1).required(),
    ArgumentSpec::string("value").minimum(1).required(),
    ArgumentSpec::string("note"),
];
static TAG_DELETE_ARGS: &[ArgumentSpec] =
    &[id_arg(), ArgumentSpec::string("key").minimum(1).required()];

pub static LIST_SPEC: OperationSpec = repository_operation!(
    "issues.list",
    "issues",
    "List current-session and repository work items.",
    SessionSelf,
    Read,
    "GET",
    "/api/repos/issues",
    Some("loom issues list"),
    described_mcp(
        "loom_issue",
        "list",
        "List work items in this session's repository."
    ),
    READ,
    LIST_ARGS
);

pub static GET_SPEC: OperationSpec = repository_operation!(
    "issues.get",
    "issues",
    "Inspect one work item and its owner status.",
    SessionSelf,
    Read,
    "GET",
    "/api/issues/{issue}",
    Some("loom issues get <issue>"),
    described_mcp(
        "loom_issue",
        "get",
        "Inspect one work item in this session's repository."
    ),
    READ,
    ID_ARGS
);

pub static CREATE_SPEC: OperationSpec = branch_operation!(
    "issues.create",
    "issues",
    "Create a session-owned work item.",
    SessionSelf,
    Write,
    "POST",
    "/api/branches/{branch}/issues",
    Some("loom issues add <title>"),
    described_mcp(
        "loom_issue",
        "add",
        "Create a work item claimed by this session's branch."
    ),
    WRITE,
    ADD_ARGS
);

pub static BACKLOG_CREATE_SPEC: OperationSpec = repository_operation!(
    "issues.backlog.create",
    "issues",
    "Create an unclaimed repository backlog item.",
    SessionSelf,
    Write,
    "POST",
    "/api/repos/issues",
    Some("loom issues add --repo <title>"),
    None,
    WRITE
);

pub static CLOSE_SPEC: OperationSpec = repository_operation!(
    "issues.close",
    "issues",
    "Close one work item.",
    SessionSelf,
    Write,
    "POST",
    "/api/issues/{issue}/close",
    Some("loom issues close <issue...>"),
    described_mcp("loom_issue", "close", "Close one repository work item."),
    WRITE,
    ID_ARGS
);

pub static REOPEN_SPEC: OperationSpec = repository_operation!(
    "issues.reopen",
    "issues",
    "Reopen one work item.",
    SessionSelf,
    Write,
    "POST",
    "/api/issues/{issue}/reopen",
    Some("loom issues reopen <issue...>"),
    described_mcp("loom_issue", "reopen", "Reopen one repository work item."),
    WRITE,
    ID_ARGS
);

pub static DELETE_SPEC: OperationSpec = repository_operation!(
    "issues.delete",
    "issues",
    "Permanently delete one work item.",
    SessionSelf,
    Destructive,
    "DELETE",
    "/api/issues/{issue}",
    Some("loom issues delete <issue...>"),
    described_mcp(
        "loom_issue",
        "delete",
        "Permanently delete one repository work item."
    ),
    WRITE,
    ID_ARGS
);

pub static TAG_SET_SPEC: OperationSpec = repository_operation!(
    "issues.tags.set",
    "issues",
    "Set one free-form work-item tag.",
    SessionSelf,
    Write,
    "PUT",
    "/api/issues/{issue}/tags/{key}",
    Some("loom issues tag set <issue...> --key <key> --value <value>"),
    described_mcp(
        "loom_issue",
        "tag_set",
        "Set one free-form tag on a repository work item."
    ),
    WRITE,
    TAG_SET_ARGS
);

pub static TAG_DELETE_SPEC: OperationSpec = repository_operation!(
    "issues.tags.delete",
    "issues",
    "Remove one free-form work-item tag.",
    SessionSelf,
    Write,
    "DELETE",
    "/api/issues/{issue}/tags/{key}",
    Some("loom issues tag delete <issue...> --key <key>"),
    described_mcp(
        "loom_issue",
        "tag_delete",
        "Remove one free-form tag from a repository work item."
    ),
    WRITE,
    TAG_DELETE_ARGS
);

/// Compatibility and bulk API used by multi-ID CLI commands. Scalar MCP
/// verbs bind to the semantic operations above.
pub static ACTIONS_SPEC: OperationSpec = repository_operation!(
    "issues.actions",
    "issues",
    "Atomically apply one action to multiple work items.",
    SessionSelf,
    Write,
    "POST",
    "/api/issues/actions",
    None,
    None,
    WRITE
);

static OPERATIONS: &[OperationSpec] = &[
    LIST_SPEC,
    GET_SPEC,
    CREATE_SPEC,
    BACKLOG_CREATE_SPEC,
    CLOSE_SPEC,
    REOPEN_SPEC,
    DELETE_SPEC,
    TAG_SET_SPEC,
    TAG_DELETE_SPEC,
    ACTIONS_SPEC,
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListInput {
    pub repo_root: String,
    pub scope: ListScope,
    pub all: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListScope {
    Repo,
    Backlog,
}

impl ListScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Backlog => "backlog",
        }
    }
}

impl std::str::FromStr for ListScope {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "repo" | "" => Ok(Self::Repo),
            "backlog" => Ok(Self::Backlog),
            other => Err(format!(
                "invalid scope '{other}' (expected 'repo' or 'backlog')"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdInput {
    pub id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInput {
    pub branch: String,
    pub request: CreateIssueReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetTagInput {
    pub id: i64,
    pub key: String,
    pub request: TagReq,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteTagInput {
    pub id: i64,
    pub key: String,
}

fn segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

typed_api_operation!(
    List,
    LIST_SPEC,
    ListInput,
    Vec<IssueView>,
    |input: &ListInput| {
        let repo_root = segment(&input.repo_root);
        Ok(OperationRequest::without_body(
            &LIST_SPEC,
            format!(
                "/api/repos/issues?repo_root={repo_root}&scope={}&all={}",
                input.scope.as_str(),
                input.all
            ),
        ))
    }
);

typed_api_operation!(Get, GET_SPEC, IdInput, IssueView, |input: &IdInput| {
    Ok(OperationRequest::without_body(
        &GET_SPEC,
        format!("/api/issues/{}", input.id),
    ))
});

typed_api_operation!(
    Create,
    CREATE_SPEC,
    CreateInput,
    IssueView,
    |input: &CreateInput| {
        OperationRequest::json(
            &CREATE_SPEC,
            format!("/api/branches/{}/issues", segment(&input.branch)),
            &input.request,
        )
    }
);

typed_api_operation!(
    CreateBacklog,
    BACKLOG_CREATE_SPEC,
    CreateRepoIssueReq,
    IssueView,
    |input: &CreateRepoIssueReq| {
        OperationRequest::json(&BACKLOG_CREATE_SPEC, "/api/repos/issues", input)
    }
);

typed_api_operation!(Close, CLOSE_SPEC, IdInput, IssueView, |input: &IdInput| {
    Ok(OperationRequest::without_body(
        &CLOSE_SPEC,
        format!("/api/issues/{}/close", input.id),
    ))
});

typed_api_operation!(
    Reopen,
    REOPEN_SPEC,
    IdInput,
    IssueView,
    |input: &IdInput| {
        Ok(OperationRequest::without_body(
            &REOPEN_SPEC,
            format!("/api/issues/{}/reopen", input.id),
        ))
    }
);

typed_api_operation!(
    Delete,
    DELETE_SPEC,
    IdInput,
    DeleteIssueResult,
    |input: &IdInput| {
        Ok(OperationRequest::without_body(
            &DELETE_SPEC,
            format!("/api/issues/{}", input.id),
        ))
    }
);

typed_api_operation!(
    SetTag,
    TAG_SET_SPEC,
    SetTagInput,
    IssueView,
    |input: &SetTagInput| {
        OperationRequest::json(
            &TAG_SET_SPEC,
            format!("/api/issues/{}/tags/{}", input.id, segment(&input.key)),
            &input.request,
        )
    }
);

typed_api_operation!(
    DeleteTag,
    TAG_DELETE_SPEC,
    DeleteTagInput,
    IssueView,
    |input: &DeleteTagInput| {
        Ok(OperationRequest::without_body(
            &TAG_DELETE_SPEC,
            format!("/api/issues/{}/tags/{}", input.id, segment(&input.key)),
        ))
    }
);

typed_api_operation!(
    Actions,
    ACTIONS_SPEC,
    IssueActionsReq,
    IssueActionsResult,
    |input: &IssueActionsReq| {
        OperationRequest::json(&ACTIONS_SPEC, "/api/issues/actions", input)
    }
);

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "issues",
        label: "Work items",
        operations: OPERATIONS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_requests_preserve_exact_rest_encoding() {
        let request = List::request(&ListInput {
            repo_root: "/tmp/a repo".to_string(),
            scope: ListScope::Repo,
            all: true,
        })
        .unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path,
            "/api/repos/issues?repo_root=%2Ftmp%2Fa%20repo&scope=repo&all=true"
        );

        let request = Get::request(&IdInput { id: 42 }).unwrap();
        assert_eq!(request.path, "/api/issues/42");

        let request = SetTag::request(&SetTagInput {
            id: 42,
            key: "review/state".to_string(),
            request: TagReq {
                value: "ready".to_string(),
                note: String::new(),
                by: Some("agent".to_string()),
            },
        })
        .unwrap();
        assert_eq!(request.method, "PUT");
        assert_eq!(request.path, "/api/issues/42/tags/review%2Fstate");
        assert_eq!(request.body.unwrap()["value"], "ready");
    }

    #[test]
    fn semantic_issue_verbs_have_distinct_routes() {
        assert_eq!(Close::SPEC.path, "/api/issues/{issue}/close");
        assert_eq!(Reopen::SPEC.path, "/api/issues/{issue}/reopen");
        assert_eq!(Delete::SPEC.method, "DELETE");
        assert_eq!(SetTag::SPEC.method, "PUT");
        assert!(Actions::SPEC.mcp.is_none());
        assert_eq!(
            super::operation_for_request("POST", "/api/issues/42/close")
                .unwrap()
                .id,
            "issues.close"
        );
        assert_eq!(
            super::operation_for_request("POST", "/api/issues/actions")
                .unwrap()
                .id,
            "issues.actions"
        );
    }
}
