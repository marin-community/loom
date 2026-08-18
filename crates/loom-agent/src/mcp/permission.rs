//! Effective Loom access and human-approved external scope requests.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::CreatePermissionRequestReq;

use super::{
    Adapter, CapabilitySet, ProjectionFuture, RemoteProjection, RemoteToolBinding, ServeFuture,
    ToolFuture,
};

const SERVER_NAME: &str = "loom_permission";
const TOOL_NAMES: [&str; 4] = ["show", "explain", "requests", "request"];
const READ_TOOLS: &[&str] = &["show", "explain", "requests"];
const REQUEST_TOOLS: &[&str] = &["request"];
const CAPABILITY_SETS: &[CapabilitySet] = &[
    CapabilitySet {
        name: "loom/permissions/read@v1",
        group: "permissions",
        version: "v1",
        description: "Inspect effective Loom operations, repository scope, and access requests.",
        tools: READ_TOOLS,
    },
    CapabilitySet {
        name: "loom/permissions/request@v1",
        group: "permissions",
        version: "v1",
        description: "Request a human-approved expansion of external session access.",
        tools: REQUEST_TOOLS,
    },
];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "permission",
    server_name: SERVER_NAME,
    description: "Effective operation grants and durable human approval requests.",
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
    super::builtin_server_config("permission")
}

fn tools() -> Value {
    super::validate_remote_tool_bindings(SERVER_NAME, &TOOL_NAMES, REMOTE_TOOLS);
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

#[derive(Debug, Deserialize)]
struct SessionArgs {
    session: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExplainArgs {
    operation: String,
}

#[derive(Debug, Deserialize)]
struct RequestsArgs {
    session: Option<String>,
    state: Option<String>,
}

fn default_mode() -> String {
    "write".to_string()
}

#[derive(Debug, Deserialize)]
struct RequestArgs {
    repository: String,
    reason: String,
    #[serde(default = "default_mode")]
    mode: String,
    session: Option<String>,
}

async fn resolve_session(client: &weaver_api::Client, session: Option<String>) -> Result<String> {
    match session {
        Some(session) if session != "self" => Ok(session),
        _ => Ok(client.self_context().await?.session_id),
    }
}

struct ShowProjection;

impl RemoteProjection for ShowProjection {
    type Operation = permission_operations::EffectiveGet;
    type Args = SessionArgs;

    const ADAPTER: &'static str = "permission";
    const TOOL: &'static str = "show";

    fn project(
        client: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<permission_operations::SessionInput> {
        Box::pin(async move {
            Ok(permission_operations::SessionInput {
                session: resolve_session(&client, args.session).await?,
            })
        })
    }

    fn present(
        _: &permission_operations::SessionInput,
        value: weaver_api::EffectivePermissionsView,
    ) -> Result<Value> {
        super::structured_result("effective Loom permissions", &value)
    }
}

struct ExplainProjection;

impl RemoteProjection for ExplainProjection {
    type Operation = permission_operations::Explain;
    type Args = ExplainArgs;

    const ADAPTER: &'static str = "permission";
    const TOOL: &'static str = "explain";

    fn project(
        _: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<permission_operations::ExplainInput> {
        Box::pin(async move {
            Ok(permission_operations::ExplainInput {
                operation: args.operation,
            })
        })
    }

    fn present(
        input: &permission_operations::ExplainInput,
        value: weaver_api::OperationView,
    ) -> Result<Value> {
        super::structured_result(&format!("operation {}", input.operation), &value)
    }
}

struct RequestsProjection;

impl RemoteProjection for RequestsProjection {
    type Operation = permission_operations::RequestsList;
    type Args = RequestsArgs;

    const ADAPTER: &'static str = "permission";
    const TOOL: &'static str = "requests";

    fn project(
        client: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<permission_operations::ListRequestsInput> {
        Box::pin(async move {
            Ok(permission_operations::ListRequestsInput {
                session: resolve_session(&client, args.session).await?,
                state: args.state,
            })
        })
    }

    fn present(
        _: &permission_operations::ListRequestsInput,
        value: Vec<weaver_api::PermissionRequestView>,
    ) -> Result<Value> {
        super::structured_result(&format!("{} permission request(s)", value.len()), &value)
    }
}

struct RequestProjection;

impl RemoteProjection for RequestProjection {
    type Operation = permission_operations::RequestsCreate;
    type Args = RequestArgs;

    const ADAPTER: &'static str = "permission";
    const TOOL: &'static str = "request";

    fn project(
        client: weaver_api::Client,
        args: Self::Args,
    ) -> ProjectionFuture<permission_operations::CreateRequestInput> {
        Box::pin(async move {
            Ok(permission_operations::CreateRequestInput {
                session: resolve_session(&client, args.session).await?,
                request: CreatePermissionRequestReq {
                    kind: "github_repository".to_string(),
                    repository: args.repository,
                    mode: args.mode,
                    reason: args.reason,
                },
            })
        })
    }

    fn present(
        _: &permission_operations::CreateRequestInput,
        value: weaver_api::PermissionRequestView,
    ) -> Result<Value> {
        super::structured_result(&format!("permission request {} pending", value.id), &value)
    }
}

const REMOTE_TOOLS: &[RemoteToolBinding] = &[
    RemoteToolBinding::new::<ShowProjection>(),
    RemoteToolBinding::new::<ExplainProjection>(),
    RemoteToolBinding::new::<RequestsProjection>(),
    RemoteToolBinding::new::<RequestProjection>(),
];

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(
        async move { super::call_remote_tool("permission", REMOTE_TOOLS, &name, arguments).await },
    )
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_separate_from_human_decision() {
        let advertised = tools();
        let names: Vec<_> = advertised
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        assert_eq!(REMOTE_TOOLS.len(), TOOL_NAMES.len());
        assert!(!names.contains(&"approve"));
        assert_eq!(
            expand_tool_set("loom/permissions/request@v1").unwrap(),
            vec!["mcp__loom_permission__request"]
        );
    }
}
