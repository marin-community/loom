//! Effective Loom access and human-approved external scope requests.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use weaver_api::CreatePermissionRequestReq;

use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

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
    weaver_api::mcp_tools_ordered(SERVER_NAME, &TOOL_NAMES)
}

async fn resolve_session(client: &weaver_api::Client, arguments: &Value) -> Result<String> {
    match super::string_argument(arguments, "session")? {
        Some(session) if session != "self" => Ok(session.to_string()),
        _ => Ok(client.self_context().await?.session_id),
    }
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_tool(&name, arguments).await })
}

async fn call_tool(name: &str, arguments: Value) -> Result<Value> {
    if !TOOL_NAMES.contains(&name) {
        bail!("unknown permission tool '{name}'");
    }
    if !super::runtime_tool_allowed(name) {
        bail!("permission tool '{name}' is not allowed by this session");
    }
    arguments
        .as_object()
        .context("permission tool arguments must be an object")?;
    let client = super::runtime_client("permission")?;
    match name {
        "show" => {
            let session = resolve_session(&client, &arguments).await?;
            let value = client.effective_permissions(&session).await?;
            super::structured_result("effective Loom permissions", &value)
        }
        "explain" => {
            let operation = super::string_argument(&arguments, "operation")?
                .context("explain requires operation")?;
            let value = client.operation(operation).await?;
            super::structured_result(&format!("operation {operation}"), &value)
        }
        "requests" => {
            let session = resolve_session(&client, &arguments).await?;
            let state = super::string_argument(&arguments, "state")?;
            let value = client.permission_requests(&session, state).await?;
            super::structured_result(&format!("{} permission request(s)", value.len()), &value)
        }
        "request" => {
            let session = resolve_session(&client, &arguments).await?;
            let repository = super::string_argument(&arguments, "repository")?
                .context("request requires repository")?;
            let reason =
                super::string_argument(&arguments, "reason")?.context("request requires reason")?;
            let mode = super::string_argument(&arguments, "mode")?.unwrap_or("write");
            let value = client
                .create_permission_request(
                    &session,
                    &CreatePermissionRequestReq {
                        kind: "github_repository".to_string(),
                        repository: repository.to_string(),
                        mode: mode.to_string(),
                        reason: reason.to_string(),
                    },
                )
                .await?;
            super::structured_result(&format!("permission request {} pending", value.id), &value)
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
    fn request_is_separate_from_human_decision() {
        let advertised = tools();
        let names: Vec<_> = advertised
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        assert!(!names.contains(&"approve"));
        assert_eq!(
            expand_tool_set("loom/permissions/request@v1").unwrap(),
            vec!["mcp__loom_permission__request"]
        );
    }
}
