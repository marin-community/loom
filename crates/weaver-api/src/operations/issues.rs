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

static OPERATIONS: &[OperationSpec] = &[
    repository_operation!(
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
    ),
    repository_operation!(
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
    ),
    branch_operation!(
        "issues.create",
        "issues",
        "Create a session-owned or repository backlog item.",
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
    ),
    repository_operation!(
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
    ),
    repository_operation!(
        "issues.close",
        "issues",
        "Close one work item.",
        SessionSelf,
        Write,
        "POST",
        "/api/issues/actions",
        Some("loom issues close <issue...>"),
        described_mcp("loom_issue", "close", "Close one repository work item."),
        WRITE,
        ID_ARGS
    ),
    repository_operation!(
        "issues.reopen",
        "issues",
        "Reopen one work item.",
        SessionSelf,
        Write,
        "POST",
        "/api/issues/actions",
        Some("loom issues reopen <issue...>"),
        described_mcp("loom_issue", "reopen", "Reopen one repository work item."),
        WRITE,
        ID_ARGS
    ),
    repository_operation!(
        "issues.delete",
        "issues",
        "Permanently delete one work item.",
        SessionSelf,
        Destructive,
        "POST",
        "/api/issues/actions",
        Some("loom issues delete <issue...>"),
        described_mcp(
            "loom_issue",
            "delete",
            "Permanently delete one repository work item."
        ),
        WRITE,
        ID_ARGS
    ),
    repository_operation!(
        "issues.tags.set",
        "issues",
        "Set one free-form work-item tag.",
        SessionSelf,
        Write,
        "POST",
        "/api/issues/actions",
        Some("loom issues tag set <issue...> --key <key> --value <value>"),
        described_mcp(
            "loom_issue",
            "tag_set",
            "Set one free-form tag on a repository work item."
        ),
        WRITE,
        TAG_SET_ARGS
    ),
    repository_operation!(
        "issues.tags.delete",
        "issues",
        "Remove one free-form work-item tag.",
        SessionSelf,
        Write,
        "POST",
        "/api/issues/actions",
        Some("loom issues tag delete <issue...> --key <key>"),
        described_mcp(
            "loom_issue",
            "tag_delete",
            "Remove one free-form tag from a repository work item."
        ),
        WRITE,
        TAG_DELETE_ARGS
    ),
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "issues",
        label: "Work items",
        operations: OPERATIONS,
    }
}
