use super::*;

const READ: &[&str] = &["loom/artifacts/read@v1"];
const WRITE: &[&str] = &["loom/artifacts/write@v1"];
const SCOPES: &[&str] = &["branch", "repo"];

const fn scope_arg() -> ArgumentSpec {
    ArgumentSpec::string("scope")
        .choices(SCOPES)
        .default_string("branch")
}

const fn name_arg() -> ArgumentSpec {
    ArgumentSpec::string("name")
        .minimum(1)
        .maximum(255)
        .required()
}

static LIST_ARGS: &[ArgumentSpec] = &[scope_arg()];
static GET_ARGS: &[ArgumentSpec] = &[
    name_arg(),
    ArgumentSpec::integer("rev").minimum(1),
    scope_arg(),
];
static WRITE_ARGS: &[ArgumentSpec] = &[
    name_arg(),
    ArgumentSpec::string("content").required(),
    ArgumentSpec::string("title").maximum(4096),
    ArgumentSpec::string("kind")
        .minimum(1)
        .default_string("markdown"),
    scope_arg(),
    ArgumentSpec::integer("base_rev").minimum(0),
];
static NAME_SCOPE_ARGS: &[ArgumentSpec] = &[name_arg(), scope_arg()];
static THREADS_ARGS: &[ArgumentSpec] = &[
    name_arg(),
    ArgumentSpec::boolean("all").default_boolean(false),
];
static COMMENT_ARGS: &[ArgumentSpec] = &[
    name_arg(),
    ArgumentSpec::integer("thread_id").minimum(1),
    ArgumentSpec::integer("base_rev").minimum(1),
    ArgumentSpec::string("quote").minimum(1),
    ArgumentSpec::string("prefix").default_string(""),
    ArgumentSpec::string("suffix").default_string(""),
    ArgumentSpec::string("body").minimum(1).required(),
];
static RESOLVE_ARGS: &[ArgumentSpec] = &[
    name_arg(),
    ArgumentSpec::integer("thread_id").minimum(1).required(),
];

static OPERATIONS: &[OperationSpec] = &[
    branch_operation!(
        "artifacts.list",
        "artifacts",
        "List branch and repository-scoped artifacts.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}/artifacts",
        Some("loom artifacts list"),
        described_mcp(
            "loom_artifact",
            "list",
            "List artifacts visible from this branch, or the repository's shared artifacts."
        ),
        READ,
        LIST_ARGS
    ),
    branch_operation!(
        "artifacts.get",
        "artifacts",
        "Read one artifact or immutable revision.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}/artifacts/{name}",
        Some("loom artifacts get <name>"),
        described_mcp(
            "loom_artifact",
            "get",
            "Get an artifact envelope, content, revisions, references, and stable dashboard URL."
        ),
        READ,
        GET_ARGS
    ),
    branch_operation!(
        "artifacts.write",
        "artifacts",
        "Create an artifact or append a guarded revision.",
        SessionSelf,
        Write,
        "PUT",
        "/api/branches/{branch}/artifacts/{name}",
        Some("loom artifacts write <name> [file]"),
        described_mcp(
            "loom_artifact",
            "write",
            "Create an artifact or append a revision. base_rev=0 guards creation; a later base_rev rejects stale edits."
        ),
        WRITE,
        WRITE_ARGS
    ),
    branch_operation!(
        "artifacts.delete",
        "artifacts",
        "Delete an artifact and its complete revision history.",
        SessionSelf,
        Destructive,
        "DELETE",
        "/api/branches/{branch}/artifacts/{name}",
        Some("loom artifacts delete <name>"),
        described_mcp(
            "loom_artifact",
            "delete",
            "Delete an artifact and its complete revision and discussion history."
        ),
        WRITE,
        NAME_SCOPE_ARGS
    ),
    branch_operation!(
        "artifacts.history",
        "artifacts",
        "List immutable artifact revisions.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}/artifacts/{name}",
        Some("loom artifacts history <name>"),
        described_mcp(
            "loom_artifact",
            "history",
            "List immutable revision metadata for one artifact, newest first."
        ),
        READ,
        NAME_SCOPE_ARGS
    ),
    branch_operation!(
        "artifacts.threads.list",
        "artifacts",
        "List anchored artifact review threads.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}/artifacts/{name}/threads",
        Some("loom artifacts threads <name>"),
        described_mcp(
            "loom_artifact",
            "threads",
            "List review threads and comments on one artifact; open threads only unless all=true."
        ),
        READ,
        THREADS_ARGS
    ),
    branch_operation!(
        "artifacts.threads.comment",
        "artifacts",
        "Start or reply to an artifact review thread.",
        SessionSelf,
        Write,
        "POST",
        "/api/branches/{branch}/artifacts/{name}/threads",
        Some("loom artifacts comment <name> <body>"),
        described_mcp(
            "loom_artifact",
            "comment",
            "Start an anchored review thread, or reply to an existing thread_id."
        ),
        WRITE,
        COMMENT_ARGS
    ),
    branch_operation!(
        "artifacts.threads.resolve",
        "artifacts",
        "Resolve an artifact review thread.",
        SessionSelf,
        Write,
        "POST",
        "/api/branches/{branch}/artifacts/{name}/threads/{thread}/resolve",
        Some("loom artifacts resolve <name> <thread>"),
        described_mcp(
            "loom_artifact",
            "resolve",
            "Resolve one artifact review thread."
        ),
        WRITE,
        RESOLVE_ARGS
    ),
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "artifacts",
        label: "Versioned deliverables",
        operations: OPERATIONS,
    }
}
