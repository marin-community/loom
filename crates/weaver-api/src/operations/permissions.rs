//! Typed access-discovery and permission-request API contracts.

use serde::{Deserialize, Serialize};

use crate::{
    CreatePermissionRequestReq, EffectivePermissionsView, OperationView, PermissionRequestView,
};

use super::*;

const READ: &[&str] = &["loom/permissions/read@v1"];
const REQUEST: &[&str] = &["loom/permissions/request@v1"];
const DECIDE: &[&str] = &["loom/permissions/decide@v1"];
const GITHUB_USE: &[&str] = &["loom/github/use@v1"];
const STATES: &[&str] = &["pending", "approved", "denied"];
const GITHUB_WRITE_MODE: &[&str] = &["write"];
pub const MAX_REASON_LEN: usize = 4_096;

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
        .maximum(MAX_REASON_LEN as i64)
        .required(),
    session_arg(),
];

pub static EFFECTIVE_GET_SPEC: OperationSpec = operation!(
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
);

pub static EXPLAIN_SPEC: OperationSpec = operation!(
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
);

pub static REQUESTS_LIST_SPEC: OperationSpec = operation!(
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
);

pub static REQUESTS_CREATE_SPEC: OperationSpec = operation!(
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
);

static OPERATIONS: &[OperationSpec] = &[
    EFFECTIVE_GET_SPEC,
    EXPLAIN_SPEC,
    REQUESTS_LIST_SPEC,
    REQUESTS_CREATE_SPEC,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInput {
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExplainInput {
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListRequestsInput {
    pub session: String,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRequestInput {
    pub session: String,
    pub request: CreatePermissionRequestReq,
}

typed_api_operation!(
    EffectiveGet,
    EFFECTIVE_GET_SPEC,
    SessionInput,
    EffectivePermissionsView,
    |input: &SessionInput| {
        Ok(OperationRequest::without_body(
            &EFFECTIVE_GET_SPEC,
            format!(
                "/api/sessions/{}/permissions",
                encode_path_segment(&input.session)
            ),
        ))
    }
);

typed_api_operation!(
    Explain,
    EXPLAIN_SPEC,
    ExplainInput,
    OperationView,
    |input: &ExplainInput| {
        Ok(OperationRequest::without_body(
            &EXPLAIN_SPEC,
            format!("/api/operations/{}", encode_path_segment(&input.operation)),
        ))
    }
);

typed_api_operation!(
    RequestsList,
    REQUESTS_LIST_SPEC,
    ListRequestsInput,
    Vec<PermissionRequestView>,
    |input: &ListRequestsInput| {
        let mut path = format!(
            "/api/sessions/{}/permission-requests",
            encode_path_segment(&input.session)
        );
        if let Some(state) = input.state.as_deref() {
            path.push_str("?state=");
            path.push_str(&encode_path_segment(state));
        }
        Ok(OperationRequest::without_body(&REQUESTS_LIST_SPEC, path))
    }
);

typed_api_operation!(
    RequestsCreate,
    REQUESTS_CREATE_SPEC,
    CreateRequestInput,
    PermissionRequestView,
    |input: &CreateRequestInput| {
        OperationRequest::json(
            &REQUESTS_CREATE_SPEC,
            format!(
                "/api/sessions/{}/permission-requests",
                encode_path_segment(&input.session)
            ),
            &input.request,
        )
    }
);

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "permissions",
        label: "Access and approvals",
        operations: OPERATIONS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_contracts_encode_path_query_and_body() {
        let explain = Explain::request(&ExplainInput {
            operation: "issues.tags.set".to_string(),
        })
        .unwrap();
        assert_eq!(explain.path, "/api/operations/issues%2Etags%2Eset");

        let list = RequestsList::request(&ListRequestsInput {
            session: "session/a".to_string(),
            state: Some("pending".to_string()),
        })
        .unwrap();
        assert_eq!(
            list.path,
            "/api/sessions/session%2Fa/permission-requests?state=pending"
        );

        let create = RequestsCreate::request(&CreateRequestInput {
            session: "self".to_string(),
            request: CreatePermissionRequestReq {
                kind: "github_repository".to_string(),
                repository: "acme/widgets".to_string(),
                mode: "write".to_string(),
                reason: "ship the change".to_string(),
            },
        })
        .unwrap();
        assert_eq!(create.method, "POST");
        assert_eq!(create.body.unwrap()["repository"], "acme/widgets");
    }
}
