//! Registry for Loom's built-in, restricted MCP adapters.
//!
//! Profiles select reviewed capability sets such as `mcp/github/comment`.
//! Loom expands those names into exact Claude permission rules when it stamps a
//! session, then derives the required adapter processes from that immutable
//! policy. Repositories can choose sets, but cannot inject executable adapter
//! configuration.

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use std::{collections::HashSet, future::Future, pin::Pin};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use weaver_api::{
    CustomMcpSnapshot, CustomMcpView, McpAdapterView, McpCapabilitySetView, McpRegistryView,
};

pub(crate) mod artifact;
pub(crate) mod channel;
pub(crate) mod context;
pub mod github;
pub(crate) mod history;
pub(crate) mod issue;
pub(crate) mod messaging;
pub(crate) mod permission;
pub(crate) mod session;

type ServeFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub(crate) type ToolFuture = Pin<Box<dyn Future<Output = Result<Value>> + Send>>;

pub(crate) struct Adapter {
    name: &'static str,
    server_name: &'static str,
    description: &'static str,
    capability_sets: fn() -> &'static [CapabilitySet],
    expand_tool_set: fn(&str) -> Option<Vec<String>>,
    is_permission_rule: fn(&str) -> bool,
    server_config: fn() -> Value,
    tools: fn() -> Value,
    serve: fn() -> ServeFuture,
}

/// Compile-time factory registration for one trusted builtin adapter.
///
/// `operation_bundle` joins this transport implementation to the neutral API
/// bundle without making `weaver-api` depend on MCP runtime types.
#[derive(Clone, Copy)]
struct AdapterFactory {
    operation_bundle: &'static str,
    build: fn() -> &'static Adapter,
}

/// A stable, provider-neutral set of MCP operations.  A set's digest is part
/// of the operator-visible contract: adding a tool requires a new versioned
/// identity rather than silently widening an unchanged profile selection.
pub(crate) struct CapabilitySet {
    pub name: &'static str,
    pub group: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub tools: &'static [&'static str],
}

pub(crate) fn builtin_permission_rule(
    server_name: &str,
    tool_names: &[&str],
    tool: &str,
) -> Option<String> {
    tool_names
        .contains(&tool)
        .then(|| format!("mcp__{server_name}__{tool}"))
}

pub(crate) fn is_builtin_permission_rule(
    server_name: &str,
    tool_names: &[&str],
    rule: &str,
) -> bool {
    tool_names
        .iter()
        .any(|tool| builtin_permission_rule(server_name, tool_names, tool).as_deref() == Some(rule))
}

pub(crate) fn expand_builtin_tool_set(
    server_name: &str,
    tool_names: &[&str],
    capability_sets: &[CapabilitySet],
    name: &str,
) -> Option<Vec<String>> {
    capability_sets
        .iter()
        .find(|set| set.name == name)
        .map(|set| {
            set.tools
                .iter()
                .map(|tool| {
                    builtin_permission_rule(server_name, tool_names, tool)
                        .expect("capability set references a registered tool")
                })
                .collect()
        })
}

pub(crate) fn builtin_server_config(adapter: &str) -> Value {
    json!({ "type": "stdio", "command": "loom", "args": ["mcp", "serve", adapter] })
}

pub(crate) fn string_argument<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        Some(value) => Ok(Some(
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .with_context(|| format!("{key} must be a non-empty string"))?,
        )),
        None => Ok(None),
    }
}

macro_rules! adapter_factories {
    ($(($name:ident, $module:ident, $bundle:literal)),+ $(,)?) => {
        $(
            fn $name() -> &'static Adapter {
                &$module::ADAPTER
            }
        )+

        const ADAPTER_FACTORIES: &[AdapterFactory] = &[
            $(
                AdapterFactory {
                    operation_bundle: $bundle,
                    build: $name,
                },
            )+
        ];
    };
}

adapter_factories!(
    (github_adapter, github, "permissions"),
    (context_adapter, context, "sessions"),
    (channel_adapter, channel, "channels"),
    (artifact_adapter, artifact, "artifacts"),
    (issue_adapter, issue, "issues"),
    (session_adapter, session, "sessions"),
    (history_adapter, history, "sessions"),
    (messaging_adapter, messaging, "sessions"),
    (permission_adapter, permission, "permissions"),
);

fn adapters() -> impl Iterator<Item = &'static Adapter> {
    ADAPTER_FACTORIES.iter().map(|factory| (factory.build)())
}

fn validate_adapter_factories() {
    weaver_api::validate_operation_bundle_coverage(
        "MCP",
        ADAPTER_FACTORIES
            .iter()
            .map(|factory| factory.operation_bundle),
    )
    .expect("invalid MCP bundle factory registry");
    let mut names = std::collections::BTreeSet::new();
    let mut servers = std::collections::BTreeSet::new();
    for registration in ADAPTER_FACTORIES {
        let adapter = (registration.build)();
        assert!(
            names.insert(adapter.name),
            "duplicate MCP adapter {}",
            adapter.name
        );
        assert!(
            servers.insert(adapter.server_name),
            "duplicate MCP server {}",
            adapter.server_name
        );
    }
    for operation in weaver_api::operations() {
        let Some(projection) = operation.mcp else {
            continue;
        };
        let registration = ADAPTER_FACTORIES
            .iter()
            .find(|registration| (registration.build)().server_name == projection.server)
            .unwrap_or_else(|| {
                panic!(
                    "operation {} projects unknown MCP server {}",
                    operation.id, projection.server
                )
            });
        assert_eq!(
            registration.operation_bundle, operation.bundle,
            "operation {} projects through an MCP factory owned by another bundle",
            operation.id
        );
        let tools = ((registration.build)().tools)();
        assert!(
            tools
                .as_array()
                .expect("builtin MCP tools must be an array")
                .iter()
                .any(|tool| tool["name"] == projection.tool),
            "operation {} projects unknown MCP tool {}::{}",
            operation.id,
            projection.server,
            projection.tool
        );
    }
}
pub(crate) const ALLOWED_TOOLS_ENV: &str = "LOOM_MCP_ALLOWED_TOOLS";
const BUILTIN_RUNTIME_ENV: [&str; 4] = [
    "WEAVER_API",
    "WEAVER_BRANCH",
    "LOOM_TOKEN",
    "LOOM_SESSION_ID",
];

#[derive(Debug, FromRow)]
struct CustomMcpRow {
    identity: String,
    group_name: String,
    label: String,
    description: String,
    enabled: bool,
    current_revision: i64,
    source: String,
    test_source: String,
    digest: String,
    tools_json: String,
    validation_state: String,
    validation_message: String,
    created_at: String,
    updated_at: String,
}

fn custom_mcp_query() -> &'static str {
    "SELECT s.identity, s.group_name, s.label, s.description, s.enabled,
            s.current_revision, r.source, r.test_source, r.digest, r.tools_json,
            r.validation_state, r.validation_message, s.created_at, s.updated_at
     FROM custom_mcp_servers s
     JOIN custom_mcp_revisions r
       ON r.identity = s.identity AND r.revision = s.current_revision"
}

fn custom_mcp_view(row: CustomMcpRow) -> Result<CustomMcpView> {
    Ok(CustomMcpView {
        identity: row.identity,
        group: row.group_name,
        label: row.label,
        description: row.description,
        enabled: row.enabled,
        revision: row.current_revision,
        digest: row.digest,
        source: row.source,
        test_source: row.test_source,
        tools: serde_json::from_str(&row.tools_json).context("invalid custom MCP tools")?,
        validation_state: row.validation_state,
        validation_message: row.validation_message,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub async fn list_custom(db: &crate::Db) -> Result<Vec<CustomMcpView>> {
    let rows =
        sqlx::query_as::<_, CustomMcpRow>(&format!("{} ORDER BY s.identity", custom_mcp_query()))
            .fetch_all(db)
            .await?;
    rows.into_iter().map(custom_mcp_view).collect()
}

pub async fn get_custom(db: &crate::Db, identity: &str) -> Result<Option<CustomMcpView>> {
    let row =
        sqlx::query_as::<_, CustomMcpRow>(&format!("{} WHERE s.identity = ?", custom_mcp_query()))
            .bind(identity)
            .fetch_optional(db)
            .await?;
    row.map(custom_mcp_view).transpose()
}

pub fn ready_custom_snapshots(items: &[CustomMcpView]) -> Vec<CustomMcpSnapshot> {
    items
        .iter()
        .filter(|item| item.enabled && item.validation_state == "ready")
        .map(|item| CustomMcpSnapshot {
            server_name: custom_server_name(&item.identity),
            identity: item.identity.clone(),
            group: item.group.clone(),
            revision: item.revision,
            digest: item.digest.clone(),
            tools: item.tools.clone(),
            source: item.source.clone(),
        })
        .collect()
}

pub fn custom_server_name(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    format!("loom_custom_{}", &hex::encode(digest)[..12])
}

pub fn custom_permission_rule(server_name: &str, tool: &str) -> String {
    format!("mcp__{server_name}__{tool}")
}

pub fn is_tool_set(name: &str) -> bool {
    adapters().any(|adapter| (adapter.expand_tool_set)(name).is_some())
}

pub fn is_builtin_group(group: &str) -> bool {
    adapters().any(|adapter| {
        (adapter.capability_sets)()
            .iter()
            .any(|set| set.group == group)
    })
}

pub fn registry() -> McpRegistryView {
    validate_adapter_factories();
    let mut adapter_views = Vec::new();
    let mut capability_sets = Vec::new();
    for registration in ADAPTER_FACTORIES {
        let adapter = (registration.build)();
        let advertised = (adapter.tools)();
        let advertised_names = advertised
            .as_array()
            .expect("builtin MCP tools must be an array")
            .iter()
            .map(|tool| {
                tool["name"]
                    .as_str()
                    .expect("builtin MCP tool must have a name")
            })
            .collect::<Vec<_>>();
        for set in (adapter.capability_sets)() {
            assert!(
                set.tools.iter().all(|tool| advertised_names.contains(tool)),
                "builtin MCP capability set {} advertises an unknown tool",
                set.name
            );
        }
        adapter_views.push(McpAdapterView {
            name: adapter.name.to_string(),
            description: adapter.description.to_string(),
            server_name: adapter.server_name.to_string(),
        });
        for set in (adapter.capability_sets)() {
            let tools = set.tools.iter().map(|tool| tool.to_string()).collect();
            capability_sets.push(McpCapabilitySetView {
                name: set.name.to_string(),
                group: set.group.to_string(),
                version: set.version.to_string(),
                digest: capability_set_digest(adapter, set, &advertised),
                description: set.description.to_string(),
                adapter: adapter.name.to_string(),
                tools,
                deprecated_by: canonical_capability_successor(set.name).map(str::to_string),
            });
        }
    }
    McpRegistryView {
        adapters: adapter_views,
        capability_sets,
        custom_servers: Vec::new(),
    }
}

fn canonical_capability_successor(name: &str) -> Option<&'static str> {
    match name {
        "mcp/github/comment@v1" => Some("loom/github/comment@v1"),
        "mcp/context/read@v1" => Some("loom/context/read@v1"),
        "mcp/channel/read@v1" => Some("loom/channels/read@v1"),
        "mcp/channel/write@v1" => Some("loom/channels/write@v1"),
        "mcp/artifact/read@v1" => Some("loom/artifacts/read@v1"),
        "mcp/artifact/write@v1" => Some("loom/artifacts/write@v1"),
        "mcp/session/read@v1" | "mcp/history/self@v1" => Some("loom/sessions/read@v1"),
        "mcp/session/status@v1" | "mcp/messaging/status@v1" => Some("loom/sessions/write@v1"),
        _ => None,
    }
}

fn selected_for_groups(set: &McpCapabilitySetView, groups: &[String]) -> bool {
    if set.deprecated_by.is_some() {
        return false;
    }
    groups.iter().any(|group| group == &set.group)
        || (set.name == "loom/sessions/read@v1" && groups.iter().any(|group| group == "history"))
        || (set.name == "loom/sessions/write@v1" && groups.iter().any(|group| group == "messaging"))
}

/// Report whether an exact profile snapshot is still launchable. Profiles
/// remain inspectable when a set is retired or a custom server is disabled,
/// but a new session must not silently substitute current registry content.
pub async fn snapshot_errors(
    db: &crate::Db,
    snapshot: &weaver_api::McpPolicySnapshot,
) -> Result<Vec<String>> {
    let current = registry();
    let custom = list_custom(db).await?;
    let mut errors = Vec::new();
    for pinned in &snapshot.capability_sets {
        match current
            .capability_sets
            .iter()
            .find(|candidate| candidate.name == pinned.name)
        {
            None => errors.push(format!(
                "built-in capability set '{}' is no longer supported",
                pinned.name
            )),
            Some(candidate) if candidate != pinned => errors.push(format!(
                "built-in capability set '{}' changed (pinned {}, current {}); save the profile to reconcile it",
                pinned.name, pinned.digest, candidate.digest
            )),
            Some(_) => {}
        }
    }
    for pinned in &snapshot.custom_servers {
        match custom
            .iter()
            .find(|candidate| candidate.identity == pinned.identity)
        {
            None => errors.push(format!(
                "custom MCP '{}' was removed; save the profile to reconcile it",
                pinned.identity
            )),
            Some(candidate) if !candidate.enabled => errors.push(format!(
                "custom MCP '{}' is disabled; enable it or save the profile to reconcile it",
                pinned.identity
            )),
            Some(_) => {}
        }
    }
    Ok(errors)
}

pub async fn resolve_access(
    db: &crate::Db,
    access: &weaver_api::McpAccess,
) -> Result<weaver_api::McpPolicySnapshot> {
    let registry = registry();
    let custom = list_custom(db).await?;
    let ready_custom = ready_custom_snapshots(&custom);
    let capability_sets = match access.mode.as_str() {
        "none" => Vec::new(),
        "all" => registry
            .capability_sets
            .into_iter()
            .filter(|set| set.deprecated_by.is_none())
            .collect(),
        "groups" => {
            for group in &access.groups {
                if !registry
                    .capability_sets
                    .iter()
                    .any(|set| &set.group == group)
                    && !custom.iter().any(|server| &server.group == group)
                {
                    bail!("unknown MCP group '{group}'");
                }
            }
            registry
                .capability_sets
                .into_iter()
                .filter(|set| selected_for_groups(set, &access.groups))
                .collect()
        }
        other => bail!("MCP access mode must be 'none', 'all', or 'groups', got '{other}'"),
    };
    let custom_servers = match access.mode.as_str() {
        "none" => Vec::new(),
        "all" => ready_custom,
        "groups" => ready_custom
            .into_iter()
            .filter(|server| access.groups.contains(&server.group))
            .collect(),
        _ => unreachable!(),
    };
    Ok(weaver_api::McpPolicySnapshot {
        selection: access.clone(),
        capability_sets,
        custom_servers,
    })
}

pub fn rules_for_snapshot(snapshot: &weaver_api::McpPolicySnapshot) -> Result<Vec<String>> {
    let names = snapshot
        .capability_sets
        .iter()
        .map(|set| set.name.clone())
        .collect::<Vec<_>>();
    let mut rules = expand_tool_sets(&names)?;
    for server in &snapshot.custom_servers {
        for tool in &server.tools {
            push_unique(
                &mut rules,
                custom_permission_rule(&server.server_name, tool),
            );
        }
    }
    Ok(rules)
}

/// Resolve the exact profile and MCP snapshot rules stamped onto a session.
pub fn effective_allowed_tool_rules_for(
    profile: &crate::profile_data::Profile,
    snapshot: &weaver_api::McpPolicySnapshot,
) -> Result<Vec<String>> {
    let mut rules = expand_tool_sets(&profile.allowed_tool_rules()?)?;
    for rule in rules_for_snapshot(snapshot)? {
        push_unique(&mut rules, rule);
    }
    Ok(rules)
}

fn capability_set_digest(adapter: &Adapter, set: &CapabilitySet, advertised: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(adapter.name);
    hasher.update([0]);
    hasher.update(adapter.server_name);
    hasher.update([0]);
    hasher.update(adapter.description);
    hasher.update([0]);
    hasher.update(set.name);
    hasher.update([0]);
    hasher.update(set.group);
    hasher.update([0]);
    hasher.update(set.version);
    hasher.update([0]);
    hasher.update(set.description);
    for tool in set.tools {
        hasher.update([0]);
        hasher.update(tool);
        if let Some(definition) = advertised
            .as_array()
            .and_then(|tools| tools.iter().find(|value| value["name"] == **tool))
        {
            hasher.update([0]);
            hasher.update(
                serde_json::to_vec(definition)
                    .expect("builtin MCP tool definitions must serialize"),
            );
        }
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Expand profile-facing capability sets into the exact rules persisted on a
/// session. Ordinary Claude rules are retained and duplicates are removed
/// without changing their order.
pub fn expand_tool_sets(rules: &[String]) -> Result<Vec<String>> {
    let mut expanded = Vec::new();
    for rule in rules {
        let tool_rules = adapters().find_map(|adapter| (adapter.expand_tool_set)(rule));
        if let Some(tool_rules) = tool_rules {
            for tool_rule in tool_rules {
                push_unique(&mut expanded, tool_rule);
            }
        } else if rule.starts_with("mcp/") || rule.starts_with("loom/") {
            bail!("unknown built-in MCP tool set '{rule}'");
        } else {
            push_unique(&mut expanded, rule.clone());
        }
    }
    Ok(expanded)
}

/// Build only the MCP server definitions needed by the session's exact
/// permission rules. Adapter commands come from this trusted registry, never
/// from repository-controlled profile data.
pub(crate) fn server_configs(allowed_rules: &[String]) -> Map<String, Value> {
    let mut servers = Map::new();
    for adapter in adapters() {
        let surface = (adapter.tools)();
        let allowed_tools = surface
            .as_array()
            .expect("builtin MCP tools must be an array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .filter(|tool| {
                allowed_rules
                    .iter()
                    .any(|rule| rule == &format!("mcp__{}__{tool}", adapter.server_name))
            })
            .collect::<Vec<_>>();
        if !allowed_tools.is_empty()
            && allowed_rules
                .iter()
                .any(|rule| (adapter.is_permission_rule)(rule))
        {
            let mut config = (adapter.server_config)();
            config
                .as_object_mut()
                .expect("builtin MCP server config must be an object")
                .insert(
                    "env".to_string(),
                    serde_json::json!({
                        "LOOM_MCP_ALLOWED_TOOLS": serde_json::to_string(&allowed_tools)
                            .expect("builtin MCP allowed tool names must serialize")
                    }),
                );
            servers.insert(adapter.server_name.to_string(), config);
        }
    }
    servers
}

fn runtime_allowed_tools() -> Option<HashSet<String>> {
    let value = std::env::var(ALLOWED_TOOLS_ENV).ok()?;
    Some(
        serde_json::from_str::<Vec<String>>(&value)
            .unwrap_or_default()
            .into_iter()
            .collect(),
    )
}

pub fn runtime_tool_allowed(name: &str) -> bool {
    runtime_allowed_tools().is_none_or(|allowed| allowed.contains(name))
}

pub fn runtime_tools(tools: Value) -> Value {
    let Some(allowed) = runtime_allowed_tools() else {
        return tools;
    };
    Value::Array(
        tools
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|name| allowed.contains(name))
            })
            .cloned()
            .collect(),
    )
}

/// Build the scoped REST client shared by Loom's resource-shaped adapters.
pub(crate) fn runtime_client(adapter: &str) -> Result<weaver_api::Client> {
    let token = weaver_api::endpoint::token_from_env()
        .with_context(|| format!("{adapter} MCP is missing its session-scoped LOOM_TOKEN"))?;
    Ok(weaver_api::Client::new(weaver_api::endpoint::base_url()).with_token(Some(token)))
}

/// MCP results expose the typed value directly while retaining a compact text
/// projection for clients that do not consume `structuredContent` yet.
pub(crate) fn structured_result<T: Serialize>(summary: &str, value: &T) -> Result<Value> {
    let value = serde_json::to_value(value)?;
    let structured = if value.is_object() {
        value
    } else {
        json!({ "items": value })
    };
    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": structured,
        "isError": false
    }))
}

/// Shared JSON-RPC stdio loop for the resource adapters. Domain modules own
/// only their schemas and REST projection; framing stays consistent.
pub(crate) async fn serve_stdio(
    server_name: &'static str,
    tools: fn() -> Value,
    call_tool: fn(&str, Value) -> ToolFuture,
) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)
            .map_err(|error| anyhow::anyhow!("invalid MCP request: {error}"))?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": request.pointer("/params/protocolVersion")
                        .and_then(Value::as_str).unwrap_or("2024-11-05"),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": server_name, "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": runtime_tools(tools()) }
            }),
            "tools/call" => {
                let name = request.pointer("/params/name").and_then(Value::as_str);
                let arguments = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match name {
                    Some(name) => match call_tool(name, arguments).await {
                        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
                        Err(error) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": format!("{error:#}") }],
                                "isError": true
                            }
                        }),
                    },
                    None => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32602, "message": "tools/call requires params.name" }
                    }),
                }
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            }),
        };
        stdout
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

pub(crate) fn server_configs_for_snapshot(
    allowed_rules: &[String],
    snapshot: Option<&weaver_api::McpPolicySnapshot>,
) -> Map<String, Value> {
    let mut servers = server_configs(allowed_rules);
    if let Some(snapshot) = snapshot {
        for custom in &snapshot.custom_servers {
            let prefix = format!("mcp__{}__", custom.server_name);
            if allowed_rules.iter().any(|rule| rule.starts_with(&prefix)) {
                servers.insert(
                    custom.server_name.clone(),
                    serde_json::json!({
                        "type": "stdio",
                        "command": "loom",
                        "args": ["mcp", "serve-custom", custom.identity],
                        "env": {
                            "LOOM_CUSTOM_MCP_SOURCE_B64":
                                base64::engine::general_purpose::STANDARD.encode(&custom.source),
                            "LOOM_MCP_ALLOWED_TOOLS":
                                serde_json::to_string(&custom.tools)
                                    .expect("custom MCP allowed tool names must serialize")
                        }
                    }),
                );
            }
        }
    }
    servers
}

/// Convert Loom's trusted server map to ACP v1's provider-neutral stdio shape.
pub fn acp_server_configs(
    allowed_rules: &[String],
    snapshot: Option<&weaver_api::McpPolicySnapshot>,
    runtime_env: &[(String, String)],
) -> Vec<Value> {
    let loom_command = std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_string))
        .unwrap_or_else(|| "loom".to_string());
    server_configs_for_snapshot(allowed_rules, snapshot)
        .into_iter()
        .map(|(name, config)| {
            let command = match config["command"].as_str().unwrap_or_default() {
                "loom" => loom_command.clone(),
                command => command.to_string(),
            };
            let mut env = config["env"]
                .as_object()
                .map(|env| {
                    env.iter()
                        .filter_map(|(name, value)| {
                            value
                                .as_str()
                                .map(|value| serde_json::json!({ "name": name, "value": value }))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if adapters().any(|adapter| adapter.server_name == name) {
                for required in BUILTIN_RUNTIME_ENV {
                    if let Some((_, value)) =
                        runtime_env.iter().rev().find(|(key, _)| key == required)
                    {
                        env.push(serde_json::json!({ "name": required, "value": value }));
                    }
                }
            }
            serde_json::json!({
                "name": name,
                "command": command,
                "args": config["args"].as_array().cloned().unwrap_or_default(),
                "env": env,
            })
        })
        .collect()
}

pub async fn serve(adapter: &str) -> Result<()> {
    let adapter = adapters()
        .find(|candidate| candidate.name == adapter)
        .ok_or_else(|| anyhow::anyhow!("unknown built-in MCP adapter '{adapter}'"))?;
    (adapter.serve)().await
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_tool_sets, server_configs};

    #[test]
    fn expands_sets_and_preserves_ordinary_rules() {
        let rules = vec![
            "Read(./**)".to_string(),
            "mcp/github/comment@v1".to_string(),
            "Read(./**)".to_string(),
        ];
        let expanded = expand_tool_sets(&rules).unwrap();
        assert_eq!(expanded[0], "Read(./**)");
        assert_eq!(expanded.len(), 7);
        assert!(expanded.contains(&"mcp__loom_github__issue_edit".to_string()));
    }

    #[test]
    fn rejects_unknown_namespaced_sets() {
        let error = expand_tool_sets(&["mcp/github/admin".to_string()]).unwrap_err();
        assert!(error.to_string().contains("unknown built-in MCP tool set"));
    }

    #[test]
    fn selects_servers_from_exact_session_permissions() {
        assert!(server_configs(&["Read(./**)".to_string()]).is_empty());
        let servers = server_configs(&["mcp__loom_github__issue_view".to_string()]);
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("loom_github"));
        assert_eq!(
            servers["loom_github"]["env"]["LOOM_MCP_ALLOWED_TOOLS"],
            "[\"issue_view\"]"
        );
    }

    #[test]
    fn registry_exposes_versioned_provider_neutral_sets() {
        let registry = super::registry();
        let set = &registry.capability_sets[0];
        assert_eq!(set.name, "loom/github/comment@v1");
        assert!(set.digest.starts_with("sha256:"));
        assert_eq!(set.tools.len(), 6);
        let legacy = registry
            .capability_sets
            .iter()
            .find(|set| set.name == "mcp/github/comment@v1")
            .unwrap();
        assert_eq!(legacy.deprecated_by.as_deref(), Some(set.name.as_str()));
    }

    #[test]
    fn builtin_capability_digests_are_stable() {
        let expected = [
            (
                "loom/github/comment@v1",
                "sha256:acfda8c46064c15a88906ab939379235510067d389173db04b92fff8c63c7775",
            ),
            (
                "mcp/github/comment@v1",
                "sha256:d18ed893185adb2817f65ee452a570d20342eafbd91a3e8f98450a0814755e78",
            ),
            (
                "loom/context/read@v1",
                "sha256:33214f2c5f8893bfd265f9ed95543de8e11bab2011b0a017891a1d19db573f33",
            ),
            (
                "mcp/context/read@v1",
                "sha256:6679d0abe8870e5b5c5e47467497335deaefe9e2770f1e90cb76ea126914a21a",
            ),
            (
                "loom/channels/read@v1",
                "sha256:c934b7ca47ad7f722d21c51cb0151c399794d591e367090fdc2e66514a7c0f87",
            ),
            (
                "loom/channels/write@v1",
                "sha256:08137846e0c8432ab6d1559216a27392497b932b64164742b0958ae782dbef59",
            ),
            (
                "mcp/channel/read@v1",
                "sha256:7c3a3677b592f456f4e5d452d630db436e59eeeca495cdc7e92dfdd99be16a5c",
            ),
            (
                "mcp/channel/write@v1",
                "sha256:6bd1bbafa1e2faa027f0c91f7422c57ea2d522b3953eec92b15996a294789a2f",
            ),
            (
                "loom/artifacts/read@v1",
                "sha256:172f77e14a2b524a74929e17de05366970e1898a57959d7e2db9b4c7f77fd5f3",
            ),
            (
                "loom/artifacts/write@v1",
                "sha256:0ee20dc115eb64e24521bc09415b47caa6e650498b0a3fb2bf438a16a2fc68f9",
            ),
            (
                "mcp/artifact/read@v1",
                "sha256:9c40da46bf5e05e9e9c5c95a47c35aff74c23e730ddba869a05aea3e51b1e6de",
            ),
            (
                "mcp/artifact/write@v1",
                "sha256:62dc5764fb10efcc4012695a5da78db2c2c85849d9341378aea770a2f7df0925",
            ),
            (
                "loom/issues/read@v1",
                "sha256:8c5dc52c5f8de69572d0875c833e7b61da79e066e934538d45933a1dfe24c45a",
            ),
            (
                "loom/issues/write@v1",
                "sha256:f9985ad6e2102a394b0ab5c9f25dd9b77de4278b6f9eaa207672ba27033a5898",
            ),
            (
                "loom/sessions/read@v1",
                "sha256:42ddd7ec1bef2eb902673af4874c0915fcbc1f05aa8ad6b3d12297df929bdb7f",
            ),
            (
                "loom/sessions/write@v1",
                "sha256:95e5c8dd47258cca69044c02d68d112ead60ae79f1af1e5ea5d9c70ea85eb63f",
            ),
            (
                "mcp/session/read@v1",
                "sha256:c9afb64abcc069a94410f0a402163b754e99917bb521d319169f97f0bdea09b7",
            ),
            (
                "mcp/session/status@v1",
                "sha256:a9a46f1877397102cc8fbaddb51cfd327d2a4d202a59a2872ce624ed2bb850ac",
            ),
            (
                "mcp/history/self@v1",
                "sha256:74c0cfd56c67e911f8278d214d4be2e7e146bf7f21929ced385c83a7c59db761",
            ),
            (
                "mcp/messaging/status@v1",
                "sha256:972a36c8e9b973352faf4c8d636b418a28c83f6ce42bc654a44546b24c03368d",
            ),
            (
                "mcp/slack/message@v1",
                "sha256:e138ce624d742cb814a5239cc0711bc58618bb6cffeeeb8eeb15abebb63b3e83",
            ),
            (
                "loom/permissions/read@v1",
                "sha256:1e90e579bc0778b5d309eb1bb3a4731b49159a4837ed059722613c180e842968",
            ),
            (
                "loom/permissions/request@v1",
                "sha256:bb5d2bf3efa472d30668c5cce2001ea5e7f222c39e0f6199754182e296e6ef20",
            ),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
        let mut actual = std::collections::BTreeMap::new();
        for adapter in super::adapters() {
            let advertised = (adapter.tools)();
            for set in (adapter.capability_sets)() {
                actual.insert(
                    set.name,
                    super::capability_set_digest(adapter, set, &advertised),
                );
            }
        }
        assert_eq!(actual.len(), expected.len());
        for (name, digest) in expected {
            assert_eq!(actual.get(name).map(String::as_str), Some(digest), "{name}");
        }
    }

    #[tokio::test]
    async fn access_resolves_none_all_and_groups() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let none = super::resolve_access(
            &db,
            &weaver_api::McpAccess {
                mode: "none".into(),
                groups: vec![],
            },
        )
        .await
        .unwrap();
        assert!(none.capability_sets.is_empty());
        let github = super::resolve_access(
            &db,
            &weaver_api::McpAccess {
                mode: "groups".into(),
                groups: vec!["github".into()],
            },
        )
        .await
        .unwrap();
        assert_eq!(github.capability_sets.len(), 1);
        assert_eq!(github.capability_sets[0].name, "loom/github/comment@v1");
        assert!(super::resolve_access(
            &db,
            &weaver_api::McpAccess {
                mode: "groups".into(),
                groups: vec!["missing".into()],
            }
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn changed_builtin_content_invalidates_a_pinned_profile_snapshot() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut snapshot = super::resolve_access(
            &db,
            &weaver_api::McpAccess {
                mode: "groups".into(),
                groups: vec!["github".into()],
            },
        )
        .await
        .unwrap();
        snapshot.capability_sets[0].digest = "sha256:stale".to_string();
        let errors = super::snapshot_errors(&db, &snapshot).await.unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("save the profile to reconcile"));
    }

    #[test]
    fn every_adapter_satisfies_the_registry_contract() {
        super::validate_adapter_factories();
        for registration in super::ADAPTER_FACTORIES {
            let adapter = (registration.build)();
            let listed = (adapter.tools)();
            let names = listed
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert!(!names.is_empty(), "{} has no tools", adapter.name);
            for tool in names {
                let permission = format!("mcp__{}__{tool}", adapter.server_name);
                assert!((adapter.is_permission_rule)(&permission));
                assert!(
                    (adapter.capability_sets)()
                        .iter()
                        .any(|set| set.tools.contains(&tool)),
                    "{} tool {tool} belongs to no capability set",
                    adapter.name
                );
            }
            let config = (adapter.server_config)();
            assert_eq!(config["command"], "loom");
            assert_eq!(config["args"][0], "mcp");
        }
    }
}
