use super::*;

const READ: &[&str] = &["loom/sessions/read@v1"];
const WRITE: &[&str] = &["loom/sessions/write@v1"];
const STATUS_LEVELS: &[&str] = &["ok", "attention", "blocked"];
const HISTORY_KINDS: &[&str] = &[
    "message",
    "reasoning",
    "tool_call",
    "tool_result",
    "context",
    "event",
    "image",
];

const fn selector_arg() -> ArgumentSpec {
    ArgumentSpec::string("session")
        .minimum(1)
        .description("A visible session id. Omit or pass 'self' for this session.")
}

static SELECTOR_ARGS: &[ArgumentSpec] = &[selector_arg()];
static SEARCH_ARGS: &[ArgumentSpec] = &[
    selector_arg(),
    ArgumentSpec::string("q")
        .minimum(1)
        .maximum(1024)
        .required(),
    ArgumentSpec::string("before").minimum(1),
    ArgumentSpec::integer("limit").minimum(1).maximum(200),
    ArgumentSpec::string_list("kinds")
        .choices(HISTORY_KINDS)
        .unique_items(),
];
static HISTORY_ARGS: &[ArgumentSpec] = &[
    selector_arg(),
    ArgumentSpec::string("before").minimum(1),
    ArgumentSpec::integer("limit").minimum(1).maximum(200),
    ArgumentSpec::string_list("kinds")
        .choices(HISTORY_KINDS)
        .unique_items(),
];
static STATUS_SET_ARGS: &[ArgumentSpec] = &[
    ArgumentSpec::string("level")
        .choices(STATUS_LEVELS)
        .required(),
    ArgumentSpec::string("message").maximum(4096).required(),
];

/// Registered agent-workflow operations. Host-local commands (`server run`,
/// `setup`, login contexts, PTY attachment) deliberately remain CLI-native.
static OPERATIONS: &[OperationSpec] = &[
    operation!(
        "self.get",
        "sessions",
        "Resolve this caller's session, branch, repository, channel, and links.",
        SessionSelf,
        Read,
        "GET",
        "/api/self",
        Some("loom self"),
        described_mcp(
            "loom_context",
            "get",
            "Return this caller's session, branch, repository, default channel, dashboard URL, and canonical REST links."
        ),
        READ
    ),
    operation!(
        "sessions.summary.get",
        "sessions",
        "Return the current goal, status, inbox, artifacts, issues, and next actions.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}/summary",
        Some("loom summary [session]"),
        described_mcp(
            "loom_session",
            "summary",
            "Return one structured catch-up: goal, status, inbox, artifacts, issues, recent events, and next actions."
        ),
        READ,
        SELECTOR_ARGS
    ),
    operation!(
        "sessions.list",
        "sessions",
        "List and search visible sessions.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/search",
        Some("loom sessions list"),
        None,
        READ
    ),
    operation!(
        "sessions.get",
        "sessions",
        "Inspect one session and its branch projection.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}",
        Some("loom sessions get <session>"),
        described_mcp(
            "loom_session",
            "get",
            "Get one visible session and its branch/lifecycle metadata."
        ),
        READ,
        SELECTOR_ARGS
    ),
    operation!(
        "sessions.launch",
        "sessions",
        "Launch a child session from a task or claimed work item.",
        SessionSelf,
        Write,
        "POST",
        "/api/sessions",
        Some("loom launch <task>"),
        None,
        WRITE
    ),
    operation!(
        "sessions.send",
        "sessions",
        "Deliver a new prompt to a session.",
        SessionSelf,
        Write,
        "POST",
        "/api/sessions/{session}/send",
        Some("loom sessions send <session> <message>"),
        None,
        WRITE
    ),
    operation!(
        "sessions.interrupt",
        "sessions",
        "Interrupt a session's active turn.",
        SessionSelf,
        Write,
        "POST",
        "/api/sessions/{session}/interrupt",
        Some("loom sessions interrupt <session>"),
        None,
        WRITE
    ),
    operation!(
        "sessions.preview",
        "sessions",
        "Read a bounded terminal preview.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}/preview",
        Some("loom sessions preview <session>"),
        None,
        READ
    ),
    branch_operation!(
        "sessions.events.list",
        "sessions",
        "List recent durable session events.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}/events",
        Some("loom sessions events [session]"),
        None,
        READ
    ),
    branch_operation!(
        "sessions.events.create",
        "sessions",
        "Record a trusted agent lifecycle event.",
        SessionSelf,
        Write,
        "POST",
        "/api/branches/{branch}/events",
        Some("loom hook --event <event>"),
        None,
        WRITE
    ),
    operation!(
        "sessions.history.list",
        "sessions",
        "Page normalized session history records.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}/history",
        None,
        described_mcp(
            "loom_session",
            "history",
            "Page normalized records for one visible session, newest tail first."
        ),
        READ,
        HISTORY_ARGS
    ),
    operation!(
        "sessions.history.search",
        "sessions",
        "Search normalized session history records.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}/history/search",
        None,
        described_mcp(
            "loom_session",
            "search",
            "Case-insensitive literal search over one visible session's normalized history."
        ),
        READ,
        SEARCH_ARGS
    ),
    branch_operation!(
        "sessions.status.get",
        "sessions",
        "Read the session's durable attention level and status message.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}",
        Some("loom status get"),
        described_mcp(
            "loom_session",
            "status_get",
            "Read a session's durable attention level and current-state message."
        ),
        READ,
        SELECTOR_ARGS
    ),
    branch_operation!(
        "sessions.status.set",
        "sessions",
        "Update the durable attention level and status message.",
        SessionSelf,
        Write,
        "POST",
        "/api/branches/{branch}/status",
        Some("loom status set --tag <level> --message <text>"),
        described_mcp(
            "loom_session",
            "status_set",
            "Update this session's status projection and append a typed status item to its channel."
        ),
        WRITE,
        STATUS_SET_ARGS
    ),
    branch_operation!(
        "sessions.tags.list",
        "sessions",
        "List free-form tags on a session.",
        SessionSelf,
        Read,
        "GET",
        "/api/branches/{branch}",
        Some("loom sessions tags list"),
        None,
        READ
    ),
    branch_operation!(
        "sessions.tags.set",
        "sessions",
        "Set one free-form session tag.",
        SessionSelf,
        Write,
        "PUT",
        "/api/branches/{branch}/tags/{key}",
        Some("loom sessions tags set <key> <value>"),
        None,
        WRITE
    ),
    branch_operation!(
        "sessions.tags.delete",
        "sessions",
        "Remove one free-form session tag.",
        SessionSelf,
        Write,
        "DELETE",
        "/api/branches/{branch}/tags/{key}",
        Some("loom sessions tags delete <key>"),
        None,
        WRITE
    ),
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "sessions",
        label: "Session workflow",
        operations: OPERATIONS,
    }
}
