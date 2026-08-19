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

// All six tools gate through one registered operation via a runtime
// session-policy check, not a compile-time grant, so this adapter keeps its
// own schemas and dispatch loop instead of `dispatch::bind`. See the module
// doc on `github` for the full reasoning.
pub mod github;

pub(crate) mod channel;
pub(crate) mod context;
pub(crate) mod issue;
pub(crate) mod permission;

// Converted except for a handful of tools that stay hand-written on purpose —
// see each module's own doc comment for exactly which and why (a response
// enrichment the plain operation does not carry, or an operation that has no
// registry entry under this adapter's own server name).
pub(crate) mod artifact;
pub(crate) mod history;
pub(crate) mod messaging;
pub(crate) mod session;

pub(crate) mod dispatch;

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

const ADAPTERS: &[&Adapter] = &[
    &github::ADAPTER,
    &context::ADAPTER,
    &channel::ADAPTER,
    &artifact::ADAPTER,
    &issue::ADAPTER,
    &session::ADAPTER,
    &history::ADAPTER,
    &messaging::ADAPTER,
    &permission::ADAPTER,
];

fn adapters() -> impl Iterator<Item = &'static Adapter> {
    ADAPTERS.iter().copied()
}

fn validate_adapters() {
    let mut names = std::collections::BTreeSet::new();
    let mut servers = std::collections::BTreeSet::new();
    for adapter in adapters() {
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
    validate_adapters();
    let mut adapter_views = Vec::new();
    let mut capability_sets = Vec::new();
    for adapter in adapters() {
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
    /// The digests pin what each capability advertises to an agent.
    ///
    /// They move when a tool's schema moves, which is the point: a session
    /// launched with `loom/artifacts/write@v1` should be able to tell that the
    /// shape of `write` changed underneath it. Re-pin only with a reason.
    ///
    /// Last re-pinned for four deliberate contract fixes: `artifacts.write.kind`
    /// became optional so omitting it keeps the artifact's current kind instead
    /// of resetting it to markdown; `artifacts.threads.list` swapped an `all`
    /// flag for `open_only` so the default keeps returning resolved threads;
    /// and `issues.list` gained the `backlog` filter that `?scope=backlog` used
    /// to provide.
    ///
    /// Re-pinned again when `channel`, `artifact`, and `permission` switched
    /// their canonical (`loom/*@v1`) sets from a hand-authored tool order to
    /// `super::dispatch::derive_capability_sets`, which groups a bundle's
    /// operations by their own `grants` field and sorts each group's tools
    /// alphabetically: `loom/channels/read@v1`, `loom/channels/write@v1`,
    /// `loom/artifacts/read@v1`, `loom/artifacts/write@v1`, and
    /// `loom/permissions/read@v1` all keep the exact same tool *membership*,
    /// just reordered (e.g. artifacts' read set was `[list, get, history,
    /// threads]`, now `[get, history, list, threads]`). Their legacy `mcp/*@v1`
    /// twins, `loom/sessions/*@v1`, `loom/context/read@v1`, and
    /// `loom/permissions/request@v1` (one tool — nothing to reorder) stay
    /// hand-authored and are untouched by this re-pin; see each adapter's own
    /// module doc comment for why.
    ///
    /// Re-pinned again for two new operations joining an existing grant:
    /// `channels.archive` widened `loom/channels/write@v1` (and its
    /// `mcp/channel/write@v1` twin) by one tool, and `issues.update` widened
    /// `loom/issues/write@v1` the same way.
    ///
    /// Re-pinned again after rewriting the doc comments on `artifacts.*`,
    /// `issues.*`, and `channels.*` operation structs for clarity — those doc
    /// comments are the MCP tool description, so the digest moved with the
    /// text. No schema or behavior changed.
    ///
    /// Re-pinned again after `loom-api-macros::field::doc_comment` stopped
    /// joining an operation's entire multi-paragraph doc comment into its MCP
    /// description; it now stops at the first blank `///` line, so any
    /// operation whose doc comment went on to explain grant reasoning or a
    /// route it replaced stopped advertising that explanation to an agent.
    /// `artifacts.write`, `channels.archive`/`channels.create` and their
    /// legacy `mcp/*@v1` twins were the only descriptions that changed.
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
                "sha256:267b07a04f242f0cfae0601dc671d1e545f7c6d146e579a3e7e0ee5a86c17cde",
            ),
            (
                "mcp/context/read@v1",
                "sha256:730dfd75e29a4802def457da752aff0bebe4df1cd00259e19e691b8fd9c303d3",
            ),
            (
                "loom/channels/read@v1",
                "sha256:b9622262c7af548b291b8a492f0cb934ccbc3d154b37fb0d036921bbe2129b55",
            ),
            (
                "loom/channels/write@v1",
                "sha256:0a3c055cc0fa683d2c223ee7685aed6eec574aaf271a511cd687141079bc807d",
            ),
            (
                "mcp/channel/read@v1",
                "sha256:8e0aaa5aa62d047e71ed60f93ce28145cfd94ca083b148e9a6732951756a3d4d",
            ),
            (
                "mcp/channel/write@v1",
                "sha256:e2e84b022c03e482922a6aaf966db27fb329fb5863a762176f6ea559150b7477",
            ),
            (
                "loom/artifacts/read@v1",
                "sha256:0a335a3b93cf6137d7604fa744bcb2219799e31ee1375bf3e26488dcca7a1d9a",
            ),
            (
                "loom/artifacts/write@v1",
                "sha256:2923f6036496be067a51890d3f121016fbfbdb1d032a5710a25bad0c617e4334",
            ),
            (
                "mcp/artifact/read@v1",
                "sha256:06c5c612abcb6bc9c17c2f543276619bcc006423b6a11c1379405b76775a7179",
            ),
            (
                "mcp/artifact/write@v1",
                "sha256:898c1f154c1c9696567581eea5ada77ab5659ce6f4cfa33a6e29dbbd31b0a188",
            ),
            (
                "loom/issues/read@v1",
                "sha256:bc4524487cd476c3883a035740ceb5a747efd0645e2f89b10a86370f1002b627",
            ),
            (
                "loom/issues/write@v1",
                "sha256:3d523c1c205981a58712426e1413250163ec4b68d8e162c4ba55e7ca58851c74",
            ),
            (
                "loom/sessions/read@v1",
                "sha256:953d645382b19e57c4b594d14590c5a8e4cc1b47c8d5ad6d8b264ca3cd756468",
            ),
            (
                "loom/sessions/write@v1",
                "sha256:4da539fad4645d5eb0a1483f88612d23784f3efa4d056bd177b76accd484c5e7",
            ),
            (
                "mcp/session/read@v1",
                "sha256:bf0540c5743cb3b57128871d5db4af6ae26844d26b91aca06bea9a9afac75ae0",
            ),
            (
                "mcp/session/status@v1",
                "sha256:7f23ff2bd1a1e0c68c7f4e01776b11e5f28e759b55c6bf66c6c1d8b09f9891b2",
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
                "sha256:143a7be8de88851dfb6f2258952eb5a539552a44a95cfe0040b1b3ca92439696",
            ),
            (
                "loom/permissions/request@v1",
                "sha256:1cc1e36de9d7563301cebc1c565aa4a99409cd7fdc007ac5803c257357a6a73e",
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
        // Report EVERY drift at once. Failing on the first one hides how much
        // moved, which is exactly when re-pinning stops being a decision and
        // becomes a reflex.
        let drift: Vec<String> = expected
            .iter()
            .filter(|(name, digest)| actual.get(**name).map(String::as_str) != Some(**digest))
            .map(|(name, digest)| {
                format!(
                    "  {name}\n    was {digest}\n    now {}",
                    actual.get(*name).map(String::as_str).unwrap_or("(absent)")
                )
            })
            .collect();
        assert!(
            drift.is_empty(),
            "{} capability set(s) changed what they advertise to agents:\n{}",
            drift.len(),
            drift.join("\n")
        );
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

    #[test]
    fn every_adapter_satisfies_the_registry_contract() {
        super::validate_adapters();
        for adapter in super::adapters() {
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
