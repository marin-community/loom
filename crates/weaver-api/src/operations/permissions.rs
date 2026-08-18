use super::*;

const READ: &[&str] = &["loom/permissions/read@v1"];
const REQUEST: &[&str] = &["loom/permissions/request@v1"];
const DECIDE: &[&str] = &["loom/permissions/decide@v1"];
const GITHUB_USE: &[&str] = &["loom/github/use@v1"];
const STATES: &[&str] = &["pending", "approved", "denied"];
const GITHUB_WRITE_MODE: &[&str] = &["write"];

const fn session_arg() -> ArgumentSpec {
    ArgumentSpec::string("session")
        .minimum(1)
        .description("A visible session id. Omit or pass 'self' for this session.")
}

static SHOW_ARGS: &[ArgumentSpec] = &[session_arg()];
static EXPLAIN_ARGS: &[ArgumentSpec] = &[ArgumentSpec::string("operation").minimum(1).required()];
static REQUESTS_ARGS: &[ArgumentSpec] =
    &[session_arg(), ArgumentSpec::string("state").choices(STATES)];
static REQUEST_ARGS: &[ArgumentSpec] = &[
    ArgumentSpec::string("repository")
        .pattern("^[^/]+/[^/]+$")
        .required(),
    ArgumentSpec::string("mode")
        .choices(GITHUB_WRITE_MODE)
        .default_string("write"),
    ArgumentSpec::string("reason")
        .minimum(1)
        .maximum(4096)
        .required(),
    session_arg(),
];

static OPERATIONS: &[OperationSpec] = &[
    operation!(
        "permissions.effective.get",
        "permissions",
        "Show effective Loom operations and external repository scope.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}/permissions",
        Some("loom permissions show"),
        mcp("loom_permission", "show"),
        READ,
        SHOW_ARGS
    ),
    operation!(
        "permissions.explain",
        "permissions",
        "Explain one operation's actor, risk, and capability requirements.",
        SessionSelf,
        Read,
        "GET",
        "/api/operations/{operation}",
        Some("loom permissions explain <operation>"),
        mcp("loom_permission", "explain"),
        READ,
        EXPLAIN_ARGS
    ),
    operation!(
        "permissions.requests.list",
        "permissions",
        "List durable external-access requests for a session.",
        SessionSelf,
        Read,
        "GET",
        "/api/sessions/{session}/permission-requests",
        Some("loom permissions requests"),
        mcp("loom_permission", "requests"),
        READ,
        REQUESTS_ARGS
    ),
    operation!(
        "permissions.requests.create",
        "permissions",
        "Request a human-approved external credential expansion.",
        SessionSelf,
        Write,
        "POST",
        "/api/sessions/{session}/permission-requests",
        Some("loom permissions request github-repository <owner/repo>"),
        mcp("loom_permission", "request"),
        REQUEST,
        REQUEST_ARGS
    ),
    operation!(
        "permissions.requests.approve",
        "permissions",
        "Approve and apply a pending external-access request.",
        User,
        ExternalWrite,
        "POST",
        "/api/permission-requests/{request}/decision",
        Some("loom permissions approve <request>"),
        None,
        DECIDE
    ),
    operation!(
        "permissions.requests.deny",
        "permissions",
        "Deny a pending external-access request.",
        User,
        Write,
        "POST",
        "/api/permission-requests/{request}/decision",
        Some("loom permissions deny <request>"),
        None,
        DECIDE
    ),
    operation!(
        "permissions.github.grant",
        "permissions",
        "Directly grant one GitHub repository to a live session.",
        User,
        ExternalWrite,
        "PUT",
        "/api/sessions/{session}/github/access",
        Some("loom permissions grant github-repository <owner/repo>"),
        None,
        DECIDE
    ),
    operation!(
        "permissions.github.revoke",
        "permissions",
        "Revoke one explicit GitHub repository override.",
        User,
        ExternalWrite,
        "PUT",
        "/api/sessions/{session}/github/access",
        Some("loom permissions revoke github-repository <owner/repo>"),
        None,
        DECIDE
    ),
    operation!(
        "permissions.github.token",
        "permissions",
        "Mint a refreshable repository-scoped GitHub App credential for this session.",
        SessionSelf,
        ExternalWrite,
        "POST",
        "/api/sessions/{session}/github/token",
        Some("loom github-token"),
        None,
        GITHUB_USE
    ),
    operation!(
        "permissions.github.restricted.invoke",
        "permissions",
        "Invoke one fixed-target GitHub operation granted by restricted session policy.",
        SessionSelf,
        ExternalWrite,
        "POST",
        "/api/sessions/{session}/restricted-github/{tool}",
        None,
        None,
        GITHUB_USE
    ),
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "permissions",
        label: "Access and approvals",
        operations: OPERATIONS,
    }
}
