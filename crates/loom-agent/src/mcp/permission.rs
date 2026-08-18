//! Effective access and approval requests projected from Loom operations.

use anyhow::{Context, Result};
use serde_json::Value;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::{CreatePermissionRequestReq, OperationView};

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

async fn project_input(client: &weaver_api::Client, name: &str, arguments: Value) -> Result<Value> {
    match name {
        "show" => serde_json::to_value(permission_operations::SessionInput {
            session: super::resolve_session_argument(client, &arguments).await?,
        })
        .map_err(Into::into),
        "requests" => serde_json::to_value(permission_operations::ListRequestsInput {
            session: super::resolve_session_argument(client, &arguments).await?,
            state: arguments
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .map_err(Into::into),
        "request" => serde_json::to_value(permission_operations::CreateRequestInput {
            session: super::resolve_session_argument(client, &arguments).await?,
            request: CreatePermissionRequestReq {
                kind: "github_repository".to_string(),
                repository: super::required_string_argument(&arguments, "repository")?.to_string(),
                mode: arguments
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("write")
                    .to_string(),
                reason: super::required_string_argument(&arguments, "reason")?.to_string(),
            },
        })
        .map_err(Into::into),
        "explain" => Ok(arguments),
        _ => Ok(arguments),
    }
}

fn present(name: &str, input: &Value, output: Value) -> Result<Value> {
    match name {
        "show" => super::structured_result("effective Loom permissions", &output),
        "explain" => {
            let view: OperationView = serde_json::from_value(output)?;
            super::structured_result(&format!("operation {}", view.id), &view)
        }
        "requests" => {
            let count = output.as_array().map(Vec::len).unwrap_or_default();
            super::structured_result(&format!("{count} permission request(s)"), &output)
        }
        "request" => {
            let id = output["id"].as_str().unwrap_or("pending");
            super::structured_result(&format!("permission request {id} pending"), &output)
        }
        _ => super::structured_result(
            &format!("operation {} complete", input["operation"]),
            &output,
        ),
    }
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        weaver_api::operation_for_mcp(SERVER_NAME, &name)
            .with_context(|| format!("unknown permission tool '{name}'"))?;
        let client = super::runtime_client("permission")?;
        let input = project_input(&client, &name, arguments).await?;
        let output =
            super::call_registered_tool("permission", SERVER_NAME, &name, input.clone()).await?;
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
    fn permission_surface_is_derived_from_operation_descriptors() {
        assert_eq!(tools().as_array().unwrap().len(), TOOL_NAMES.len());
        for name in TOOL_NAMES {
            assert!(weaver_api::operation_for_mcp(SERVER_NAME, name).is_some());
        }
    }

    #[test]
    fn request_capability_remains_separate_from_human_decision() {
        assert_eq!(
            expand_tool_set("loom/permissions/request@v1")
                .unwrap()
                .len(),
            1
        );
        assert!(weaver_api::operation("permissions.requests.approve")
            .unwrap()
            .mcp
            .is_none());
    }
}
