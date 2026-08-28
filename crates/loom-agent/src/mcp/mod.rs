//! Registry and aggregate server for Loom's built-in MCP tool domains.
//!
//! Profiles select reviewed capability sets such as `loom/github/comment@v1`.
//! Loom expands those names into exact Claude permission rules when it stamps a
//! session, then exposes the selected domains through one built-in MCP server.
//! Repositories can choose sets, but cannot inject executable adapter
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
    CustomMcpSnapshot, CustomMcpView, McpCapabilitySetView, McpDomainView, McpRegistryView,
};

// All six tools gate through one registered operation via a runtime
// session-policy check, not a compile-time grant, so this domain keeps its
// own schemas and call handler instead of `dispatch::bind`. See the module
// doc on `github` for the full reasoning.
pub mod github;

pub(crate) mod channel;
pub(crate) mod context;
pub(crate) mod issue;
pub(crate) mod messaging;
pub(crate) mod permission;

// These stay hand-written rather than bound via `dispatch::bind` — see each
// module's own doc comment for exactly which tool and why (a response
// enrichment the plain operation does not carry, or an operation that has no
// direct generic rendering path).
pub(crate) mod artifact;
pub(crate) mod session;

pub(crate) mod dispatch;

pub(crate) type ToolFuture = Pin<Box<dyn Future<Output = Result<Value>> + Send>>;

pub(crate) struct Adapter {
    name: &'static str,
    description: &'static str,
    capability_sets: fn() -> &'static [CapabilitySet],
    /// The tools this adapter exports and the operation each one is.
    exports: fn() -> &'static [dispatch::Export],
    expand_tool_set: fn(&str) -> Option<Vec<String>>,
    tools: fn() -> Value,
    call: fn(&str, Value) -> ToolFuture,
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

fn builtin_tool_name(adapter: &str, tool: &str) -> String {
    format!("{adapter}_{tool}")
}

pub(crate) fn builtin_permission_rule(adapter: &str, tool: &str) -> String {
    format!(
        "mcp__{BUILTIN_SERVER_NAME}__{}",
        builtin_tool_name(adapter, tool)
    )
}

fn rule_allows_builtin_tool(rule: &str, adapter: &Adapter, tool: &str) -> bool {
    rule == builtin_permission_rule(adapter.name, tool)
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
    &messaging::ADAPTER,
    &permission::ADAPTER,
];

fn adapters() -> impl Iterator<Item = &'static Adapter> {
    ADAPTERS.iter().copied()
}

fn validate_adapters() {
    let mut names = std::collections::BTreeSet::new();
    for adapter in adapters() {
        assert!(
            names.insert(adapter.name),
            "duplicate MCP adapter {}",
            adapter.name
        );
    }
}
pub(crate) const ALLOWED_TOOLS_ENV: &str = "LOOM_MCP_ALLOWED_TOOLS";
const BUILTIN_SERVER_NAME: &str = "loom";
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
    let mut domain_views = Vec::new();
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
        domain_views.push(McpDomainView {
            name: adapter.name.to_string(),
            description: adapter.description.to_string(),
            server_name: BUILTIN_SERVER_NAME.to_string(),
        });
        for set in (adapter.capability_sets)() {
            let tools = set.tools.iter().map(|tool| tool.to_string()).collect();
            capability_sets.push(McpCapabilitySetView {
                name: set.name.to_string(),
                group: set.group.to_string(),
                version: set.version.to_string(),
                digest: capability_set_digest(adapter, set, &advertised),
                description: set.description.to_string(),
                domain: adapter.name.to_string(),
                tools,
            });
        }
    }
    McpRegistryView {
        domains: domain_views,
        capability_sets,
        custom_servers: Vec::new(),
    }
}

/// The registry grants a capability set confers.
///
/// A set is a name for some of a domain's exports, so the grants it carries are
/// the grants those operations declare.
fn capability_set_grants(name: &str) -> Vec<&'static str> {
    let mut grants = Vec::new();
    for adapter in adapters() {
        let Some(set) = (adapter.capability_sets)()
            .iter()
            .find(|set| set.name == name)
        else {
            continue;
        };
        let exports = (adapter.exports)();
        for tool in set.tools {
            let Some(export) = exports.iter().find(|export| export.tool == *tool) else {
                // A hand-written tool that is not a registered operation — the
                // restricted GitHub tools, whose boundary is the session's own
                // allow-list rather than a grant.
                continue;
            };
            for grant in export.operation.grants {
                if !grants.contains(grant) {
                    grants.push(grant);
                }
            }
        }
        break;
    }
    grants
}

/// Every registry grant a session holding `sets` may exercise.
///
/// An unrestricted session reaches everything an agent may call; a restricted
/// one reaches the base three plus whatever its selected sets confer.
pub fn session_capabilities<'a>(
    restricted: bool,
    sets: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    if !restricted {
        return weaver_api::all_session_capabilities();
    }
    let mut capabilities = std::collections::BTreeSet::from([
        "loom/sessions/read@v1".to_string(),
        "loom/permissions/read@v1".to_string(),
        "loom/permissions/request@v1".to_string(),
    ]);
    for set in sets {
        let grants = capability_set_grants(set);
        if grants.is_empty() && set.starts_with("loom/") {
            // A policy naming a grant directly rather than a set that confers it.
            capabilities.insert(set.to_string());
            continue;
        }
        capabilities.extend(grants.into_iter().map(str::to_string));
    }
    capabilities.into_iter().collect()
}

fn selected_for_groups(set: &McpCapabilitySetView, groups: &[String]) -> bool {
    groups.iter().any(|group| group == &set.group)
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
        "all" => registry.capability_sets.into_iter().collect(),
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
        .filter(|set| is_tool_set(&set.name))
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
    hasher.update(BUILTIN_SERVER_NAME);
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
        hasher.update(builtin_tool_name(adapter.name, tool));
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
            continue;
        } else {
            push_unique(&mut expanded, rule.clone());
        }
    }
    Ok(expanded)
}

/// Build the one builtin MCP server definition when the session may use any
/// builtin tool. Domain modules remain the registry and dispatch boundary;
/// they share one stdio transport so an agent session pays one startup
/// handshake rather than one per domain.
pub(crate) fn server_configs(allowed_rules: &[String]) -> Map<String, Value> {
    let mut servers = Map::new();
    let mut allowed_tools = Vec::new();
    for adapter in adapters() {
        let surface = (adapter.tools)();
        for tool in surface
            .as_array()
            .expect("builtin MCP tools must be an array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
        {
            if allowed_rules
                .iter()
                .any(|rule| rule_allows_builtin_tool(rule, adapter, tool))
            {
                allowed_tools.push(builtin_tool_name(adapter.name, tool));
            }
        }
    }
    if !allowed_tools.is_empty() {
        servers.insert(
            BUILTIN_SERVER_NAME.to_string(),
            json!({
                "type": "stdio",
                "command": "loom",
                "args": ["mcp", "serve"],
                "env": {
                    "LOOM_MCP_ALLOWED_TOOLS": serde_json::to_string(&allowed_tools)
                        .expect("builtin MCP allowed tool names must serialize")
                }
            }),
        );
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

pub fn runtime_adapter_tool_allowed(adapter: &str, tool: &str) -> bool {
    runtime_allowed_tools()
        .is_none_or(|allowed| allowed.contains(&builtin_tool_name(adapter, tool)))
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

fn builtin_tools() -> Value {
    let mut tools = Vec::new();
    for adapter in adapters() {
        for mut tool in (adapter.tools)()
            .as_array()
            .expect("builtin MCP tools must be an array")
            .iter()
            .cloned()
        {
            let local_name = tool["name"]
                .as_str()
                .expect("builtin MCP tool must have a name");
            tool["name"] = Value::String(builtin_tool_name(adapter.name, local_name));
            tools.push(tool);
        }
    }
    Value::Array(tools)
}

fn call_builtin_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move { call_builtin_tool(&name, arguments).await })
}

async fn call_builtin_tool(name: &str, arguments: Value) -> Result<Value> {
    if !runtime_tool_allowed(name) {
        bail!("builtin Loom tool '{name}' is not allowed by this session");
    }
    for adapter in adapters() {
        let Some(local_name) = name
            .strip_prefix(adapter.name)
            .and_then(|suffix| suffix.strip_prefix('_'))
        else {
            continue;
        };
        let known = (adapter.tools)()
            .as_array()
            .expect("builtin MCP tools must be an array")
            .iter()
            .any(|tool| tool["name"].as_str() == Some(local_name));
        if known {
            return (adapter.call)(local_name, arguments).await;
        }
    }
    bail!("unknown builtin Loom tool '{name}'")
}

/// Build the scoped REST client shared by Loom's resource domains.
pub(crate) fn runtime_client(adapter: &str) -> Result<weaver_api::Client> {
    let token = weaver_api::endpoint::token_from_env()
        .with_context(|| format!("{adapter} MCP is missing its session-scoped LOOM_TOKEN"))?;
    Ok(weaver_api::Client::new(weaver_api::endpoint::base_url()).with_token(Some(token)))
}

/// MCP results expose the typed value directly while retaining a compact text
/// summary for clients that do not consume `structuredContent` yet.
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

/// Shared JSON-RPC stdio loop for the aggregate server. Domain modules own
/// only their schemas and REST calls; framing stays consistent.
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

/// Convert Loom's trusted server map to ACP v1's provider-neutral stdio config.
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
            if name == BUILTIN_SERVER_NAME {
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

pub async fn serve() -> Result<()> {
    serve_stdio(BUILTIN_SERVER_NAME, builtin_tools, call_builtin_boxed).await
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{builtin_tools, expand_tool_sets, server_configs};

    #[test]
    fn expands_sets_and_preserves_ordinary_rules() {
        let rules = vec![
            "Read(./**)".to_string(),
            "loom/github/comment@v1".to_string(),
            "Read(./**)".to_string(),
        ];
        let expanded = expand_tool_sets(&rules).unwrap();
        assert_eq!(expanded[0], "Read(./**)");
        assert_eq!(expanded.len(), 7);
        assert!(expanded.contains(&"mcp__loom__github_issue_edit".to_string()));
    }

    #[test]
    fn ignores_unknown_namespaced_sets() {
        let expanded = expand_tool_sets(&[
            "mcp/github/admin".to_string(),
            "loom/retired/read@v1".to_string(),
            "Read(./**)".to_string(),
        ])
        .unwrap();
        assert_eq!(expanded, ["Read(./**)".to_string()]);
    }

    #[test]
    fn ignores_removed_capabilities_in_stored_policy() {
        let mut removed = super::registry().capability_sets[0].clone();
        removed.name = "loom/retired/read@v1".to_string();
        let snapshot = weaver_api::McpPolicySnapshot {
            capability_sets: vec![removed],
            ..Default::default()
        };
        assert!(super::rules_for_snapshot(&snapshot).unwrap().is_empty());
    }

    #[test]
    fn selects_one_server_from_current_session_permissions() {
        assert!(server_configs(&["Read(./**)".to_string()]).is_empty());
        assert!(server_configs(&["mcp__loom_channel__list".to_string()]).is_empty());
        let servers = server_configs(&[
            "mcp__loom__channel_list".to_string(),
            "mcp__loom__github_issue_view".to_string(),
        ]);
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("loom"));
        assert_eq!(
            servers["loom"]["env"]["LOOM_MCP_ALLOWED_TOOLS"],
            "[\"github_issue_view\",\"channel_list\"]"
        );
        assert_eq!(servers["loom"]["args"], serde_json::json!(["mcp", "serve"]));
    }

    #[test]
    fn aggregate_surface_names_every_tool_by_domain() {
        let tools = builtin_tools();
        let names = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), tools.as_array().unwrap().len());
        assert!(names.contains("github_issue_view"));
        assert!(names.contains("channel_send"));
        assert!(names.contains("artifact_get"));
    }

    #[test]
    fn registry_exposes_versioned_provider_neutral_sets() {
        let registry = super::registry();
        let set = &registry.capability_sets[0];
        assert_eq!(set.name, "loom/github/comment@v1");
        assert!(set.digest.starts_with("sha256:"));
        assert_eq!(set.tools.len(), 6);
        assert!(registry
            .capability_sets
            .iter()
            .all(|set| set.name.starts_with("loom/")));
        assert!(registry
            .capability_sets
            .iter()
            .any(|set| set.name == "loom/messaging/slack@v1"));
        assert!(registry
            .domains
            .iter()
            .all(|adapter| adapter.server_name == "loom"));
    }

    #[test]
    /// The digests pin what each capability advertises to an agent: they move
    /// when a tool's schema changes, so a session holding e.g.
    /// `loom/artifacts/write@v1` can tell that `write`'s tool definition
    /// changed under it. Re-pin only with a reason, and check whether tool
    /// *membership* moved too.
    fn builtin_capability_digests_are_stable() {
        let expected = [
            (
                "loom/github/comment@v1",
                "sha256:b05d602bb199e3ca1a0b8c807ecb3b84a0a1f7b673a080455b1822368a4b907e",
            ),
            (
                "loom/context/read@v1",
                "sha256:323c94d22bc51696bdedb4a84e4c04f174a46501beb2eb1ea8e19ca6ce349316",
            ),
            (
                "loom/channels/read@v1",
                "sha256:f4d4f6a80605a8784b068dc286bfcccef63f55c0b451c00f9ac301d111f43cc0",
            ),
            (
                "loom/channels/write@v1",
                "sha256:e928a720610385e4820bc5b5414b52e5a7a059c383bfd7ecc7c3fda88517da06",
            ),
            (
                "loom/artifacts/read@v1",
                "sha256:e2196b45b3727a2993f8c6fb29e1c28d067cbcd28236357b1120e88e175cb0c2",
            ),
            (
                "loom/artifacts/write@v1",
                "sha256:517436e95f976e259d652d3d969cf8f2312230223ba44a5c9f24e080643e370b",
            ),
            (
                "loom/issues/read@v1",
                "sha256:37e9afd58241a3dc68edde6a4521b02afdd0f445d4eae6910da54fea05763803",
            ),
            (
                "loom/issues/write@v1",
                "sha256:1d3b61b7ef1f3e4f49ff403c1228cc2cab45c18d9108bb7cf1a76073701816fa",
            ),
            (
                "loom/sessions/read@v1",
                "sha256:ea41b2aabb6a64e6a8738109c0d56b13363eb3e5d5f2c7aa44ce19e43e1b4752",
            ),
            (
                "loom/sessions/write@v1",
                "sha256:021c51cdef86f5a7a718295d78417769756dc22f1a826636afa56e654a7d679d",
            ),
            (
                "loom/messaging/slack@v1",
                "sha256:89d1f86e4c28cf1c73287493609abbf347ff99a257e611107e62067840b854f4",
            ),
            (
                "loom/permissions/read@v1",
                "sha256:2e789d878d398cc3711aa398c64bb312d449237ff3bd2c15a414239fd68058c7",
            ),
            (
                "loom/permissions/request@v1",
                "sha256:e2d2e7c103d995b9d8a9374f77ba4e5f03d6ad34c26fd8478ff8c4ae3df1479d",
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
        // Report every drift at once, not just the first failure, so a re-pin
        // is a deliberate decision informed by everything that changed.
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
                assert!(
                    (adapter.capability_sets)()
                        .iter()
                        .any(|set| set.tools.contains(&tool)),
                    "{} tool {tool} belongs to no capability set",
                    adapter.name
                );
            }
        }
    }
}
