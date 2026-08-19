//! Loom's operation registry.
//!
//! Every operation that reaches the Loom API is declared here exactly once, and
//! REST, the CLI, MCP, and the SPA are projections of those declarations rather
//! than parallel catalogues. The rule is short enough to state in one line:
//!
//! > Anything that reaches the API is registered. The only axis that varies is
//! > response encoding.
//!
//! *Who* may call an operation is [`ActorPolicy`], a field — administrative and
//! human-only actions are registered with `Admin`/`User`, not omitted. *How* it
//! answers is [`Io`], also a field — streams and uploads keep their descriptor,
//! typed input, and authorization, and differ only in encoding.
//!
//! Commands that never reach the API (`loom server run`, `setup`, shell
//! completions, the server-free half of `config`) are not operations and have no
//! entry here.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod registry;
pub use registry::*;

/// What an operation module imports. Keeping it one glob makes the per-operation
/// files short enough that the declaration is the first thing you read.
pub mod prelude {
    pub use super::registry::*;
    pub use crate::dto::*;
    pub use loom_api_macros::{operation, Operands, View};
    pub use schemars::JsonSchema;
    pub use serde::{Deserialize, Serialize};
}

pub mod agents;
pub mod artifacts;
pub mod auth;
pub mod branches;
pub mod channels;
pub mod deployment;
pub mod events;
pub mod issues;
pub mod logs;
pub mod mcps;
pub mod permissions;
pub mod profiles;
pub mod repos;
pub mod reviews;
pub mod runs;
pub mod session_layout;
pub mod sessions;
pub mod settings;
pub mod shell;
pub mod tasks;
pub mod watches;

/// One first-party resource group.
#[derive(Debug, Clone, Copy)]
pub struct OperationBundle {
    pub name: &'static str,
    pub label: &'static str,
    pub operations: &'static [&'static OperationSpec],
}

pub type OperationBundleFactory = fn() -> OperationBundle;

pub static OPERATION_BUNDLE_FACTORIES: &[OperationBundleFactory] = &[
    issues::bundle,
    artifacts::bundle,
    channels::bundle,
    sessions::bundle,
    permissions::bundle,
    watches::bundle,
    runs::bundle,
    tasks::bundle,
    settings::bundle,
    profiles::bundle,
    deployment::bundle,
    mcps::bundle,
    auth::bundle,
    agents::bundle,
    branches::bundle,
    repos::bundle,
    reviews::bundle,
    session_layout::bundle,
    events::bundle,
    logs::bundle,
    shell::bundle,
];

pub fn operation_bundles() -> impl Iterator<Item = OperationBundle> {
    OPERATION_BUNDLE_FACTORIES.iter().map(|factory| factory())
}

pub fn operations() -> impl Iterator<Item = &'static OperationSpec> {
    operation_bundles().flat_map(|bundle| bundle.operations.iter().copied())
}

pub fn operations_for_bundle(bundle: &str) -> impl Iterator<Item = &'static OperationSpec> + '_ {
    operations().filter(move |operation| operation.bundle == bundle)
}

pub fn operation(id: &str) -> Option<&'static OperationSpec> {
    operations().find(|operation| operation.id == id)
}

pub fn operation_for_mcp(server: &str, tool: &str) -> Option<&'static OperationSpec> {
    operations().find(|operation| {
        operation
            .mcp
            .is_some_and(|mcp| mcp.server == server && mcp.tool == tool)
    })
}

/// Resolve a canonical operation route back to its descriptor.
///
/// Routes are derived from identity, so this is an exact inverse of
/// [`OperationSpec::path`] rather than a pattern match over a route table.
pub fn operation_for_request(method: &str, path: &str) -> Option<&'static OperationSpec> {
    let path = path.strip_prefix("/api").unwrap_or(path);
    operations().find(|operation| {
        operation.method() == method
            && operation
                .path()
                .strip_prefix("/api")
                .is_some_and(|candidate| candidate == path)
    })
}

pub fn operation_input_schema(operation: &OperationSpec) -> Value {
    (operation.schema)()
}

// -- Discovery views ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpProjectionView {
    pub server: String,
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationView {
    pub id: String,
    pub bundle: String,
    pub summary: String,
    pub actor: ActorPolicy,
    pub scope: OperationScope,
    pub risk: OperationRisk,
    pub io: Io,
    pub method: String,
    pub path: String,
    pub cli: Option<String>,
    pub cli_aliases: Vec<String>,
    pub mcp: Option<McpProjectionView>,
    pub grants: Vec<String>,
    pub schema: Value,
}

impl From<&OperationSpec> for OperationView {
    fn from(spec: &OperationSpec) -> Self {
        Self {
            id: spec.id.to_string(),
            bundle: spec.bundle.to_string(),
            summary: spec.summary.to_string(),
            actor: spec.actor,
            scope: spec.scope,
            risk: spec.risk,
            io: spec.io,
            method: spec.method().to_string(),
            path: spec.path(),
            cli: spec.cli.map(|cli| cli.invocation()),
            cli_aliases: spec
                .cli
                .map(|cli| {
                    cli.aliases
                        .iter()
                        .map(|alias| (*alias).to_string())
                        .collect()
                })
                .unwrap_or_default(),
            mcp: spec.mcp.map(|mcp| McpProjectionView {
                server: mcp.server.to_string(),
                tool: mcp.tool.to_string(),
            }),
            grants: spec
                .grants
                .iter()
                .map(|grant| (*grant).to_string())
                .collect(),
            schema: (spec.schema)(),
        }
    }
}

pub fn operation_views() -> Vec<OperationView> {
    operations().map(OperationView::from).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiMetaView {
    pub product: String,
    pub version: String,
    pub operation_registry_version: u32,
    pub operations_url: String,
    pub openapi_url: String,
}

// -- MCP projection ----------------------------------------------------------

/// Generate one MCP server's tool catalogue from the registry.
///
/// An operation reaches MCP only if it declares a projection, and
/// [`validate_operation_registry`] refuses a projection on anything an agent may
/// not call — so a human-only operation cannot acquire a tool by accident.
pub fn mcp_tools(server: &str) -> Value {
    let mut tools = operations()
        .filter_map(|operation| {
            let mcp = operation.mcp.filter(|mcp| mcp.server == server)?;
            Some((
                mcp.tool,
                json!({
                    "name": mcp.tool,
                    "description": operation.summary,
                    "inputSchema": (operation.schema)(),
                }),
            ))
        })
        .collect::<Vec<_>>();
    tools.sort_by_key(|(name, _)| *name);
    Value::Array(tools.into_iter().map(|(_, tool)| tool).collect())
}

/// One server's catalogue in an explicit advertised order.
///
/// Order is observable to MCP clients, so it stays declared even though tool
/// identity is keyed by name.
pub fn mcp_tools_ordered(server: &str, order: &[&str]) -> Value {
    let by_name = mcp_tools(server)
        .as_array()
        .expect("generated MCP catalogue is an array")
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap_or_default().to_string(),
                tool.clone(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    Value::Array(
        order
            .iter()
            .map(|name| {
                by_name
                    .get(*name)
                    .unwrap_or_else(|| panic!("unregistered MCP tool {server}::{name}"))
                    .clone()
            })
            .collect(),
    )
}

// -- Capabilities ------------------------------------------------------------

/// Every grant named by a session-reachable operation.
pub fn all_session_capabilities() -> Vec<String> {
    let mut grants = operations()
        .filter(|operation| operation.actor.agent_reachable())
        .flat_map(|operation| operation.grants.iter().map(|grant| (*grant).to_string()))
        .collect::<Vec<_>>();
    grants.sort();
    grants.dedup();
    grants
}

/// Translate legacy `mcp/*@v1` capability names into registry grants.
///
/// Compatibility shim: sessions launched before the registry owned the
/// capability vocabulary still carry transport-shaped names. Delete once no live
/// session does.
pub fn session_capabilities_from_mcp<'a>(
    restricted: bool,
    capability_sets: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    if !restricted {
        return all_session_capabilities();
    }
    let mut capabilities = std::collections::BTreeSet::from([
        "loom/sessions/read@v1".to_string(),
        "loom/permissions/read@v1".to_string(),
        "loom/permissions/request@v1".to_string(),
    ]);
    for capability in capability_sets {
        let canonical = match capability {
            "mcp/context/read@v1" | "mcp/history/self@v1" | "mcp/session/read@v1" => {
                Some("loom/sessions/read@v1")
            }
            "mcp/session/status@v1" | "mcp/messaging/status@v1" => Some("loom/sessions/write@v1"),
            "mcp/channel/read@v1" => Some("loom/channels/read@v1"),
            "mcp/channel/write@v1" => Some("loom/channels/write@v1"),
            "mcp/artifact/read@v1" => Some("loom/artifacts/read@v1"),
            "mcp/artifact/write@v1" => Some("loom/artifacts/write@v1"),
            "mcp/issue/read@v1" => Some("loom/issues/read@v1"),
            "mcp/issue/write@v1" => Some("loom/issues/write@v1"),
            other if other.starts_with("loom/") => Some(other),
            _ => None,
        };
        if let Some(canonical) = canonical {
            capabilities.insert(canonical.to_string());
        }
    }
    capabilities.into_iter().collect()
}

// -- Validation --------------------------------------------------------------

/// Enforce the registry's structural invariants.
///
/// Runs at server startup as well as in tests. The registry this replaces
/// validated only that CLI strings were *unique*, never that they parsed — which
/// is how it shipped three advertised commands that did not exist.
pub fn validate_operation_registry() -> Result<(), String> {
    let mut bundle_names = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut cli_paths = std::collections::BTreeSet::new();
    let mut mcp_tools = std::collections::BTreeSet::new();

    for bundle in operation_bundles() {
        if !bundle_names.insert(bundle.name) {
            return Err(format!("duplicate operation bundle {}", bundle.name));
        }
        if bundle.operations.is_empty() {
            return Err(format!("operation bundle {} is empty", bundle.name));
        }
        for operation in bundle.operations {
            if operation.bundle != bundle.name {
                return Err(format!(
                    "operation {} declares bundle {} but was registered under {}",
                    operation.id, operation.bundle, bundle.name
                ));
            }
            if !ids.insert(operation.id) {
                return Err(format!("duplicate operation id {}", operation.id));
            }
            if let Some(cli) = operation.cli {
                if !cli_paths.insert(cli.path) {
                    return Err(format!("duplicate CLI projection {}", cli.invocation()));
                }
            }
            if let Some(mcp) = operation.mcp {
                if !mcp_tools.insert((mcp.server, mcp.tool)) {
                    return Err(format!(
                        "duplicate MCP projection {}::{}",
                        mcp.server, mcp.tool
                    ));
                }
                // The human-only boundary, enforced rather than merely absent.
                if !operation.actor.agent_reachable() {
                    return Err(format!(
                        "operation {} is {} but exposes MCP tool {}::{}; \
                         only session-self operations may reach an agent",
                        operation.id,
                        operation.actor.as_str(),
                        mcp.server,
                        mcp.tool
                    ));
                }
                // A streaming or duplex operation cannot be served by the JSON
                // dispatcher, so it must not advertise a tool for one.
                if !operation.io.is_json() {
                    return Err(format!(
                        "operation {} is io={} and cannot expose an MCP tool",
                        operation.id,
                        operation.io.as_str()
                    ));
                }
            }
            if operation.grants.is_empty() && operation.actor.agent_reachable() {
                return Err(format!(
                    "session-reachable operation {} names no grant",
                    operation.id
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_structurally_valid() {
        validate_operation_registry().unwrap();
    }

    #[test]
    fn every_operation_resolves_from_its_own_route() {
        for operation in operations() {
            let found = operation_for_request(operation.method(), &operation.path())
                .unwrap_or_else(|| panic!("{} does not resolve from its route", operation.id));
            assert_eq!(found.id, operation.id);
        }
    }
}

// -- OpenAPI projection ------------------------------------------------------

/// Render the registry as an OpenAPI 3.1 document.
///
/// Routes are unique by construction, so this is a straight map over the
/// registry. The generator it replaces had to merge colliding `path` + `method`
/// pairs into an `x-loom-operation-ids` array — a symptom of descriptors that
/// declared their own routes — and it emitted no request schema at all, because
/// `ArgumentSpec` could not describe one.
pub fn openapi_document(version: &str) -> Value {
    let mut paths = serde_json::Map::new();
    for operation in operations() {
        let body = json!({
            "required": true,
            "content": { "application/json": { "schema": (operation.schema)() } },
        });
        let mut definition = json!({
            "operationId": operation.id,
            "summary": operation.summary,
            "tags": [operation.bundle],
            "x-loom-actor": operation.actor.as_str(),
            "x-loom-scope": operation.scope.as_str(),
            "x-loom-risk": operation.risk.as_str(),
            "x-loom-io": operation.io.as_str(),
            "x-loom-grants": operation.grants,
            "responses": { "200": { "description": "success" } },
        });
        if let Some(cli) = operation.cli {
            definition["x-loom-cli"] = json!(cli.invocation());
        }
        if let Some(mcp) = operation.mcp {
            definition["x-loom-mcp"] = json!(format!("{}::{}", mcp.server, mcp.tool));
        }
        if operation.method() == "POST" {
            definition["requestBody"] = body;
        }
        let method = operation.method().to_ascii_lowercase();
        paths
            .entry(operation.path())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("operation path object")
            .insert(method, definition);
    }
    json!({
        "openapi": "3.1.0",
        "info": { "title": "Loom API", "version": version },
        "paths": paths,
    })
}
