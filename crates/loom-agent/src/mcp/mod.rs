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

// These stay hand-written rather than bound via `dispatch::bind` — see each
// module's own doc comment for exactly which tool and why (a response
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
    /// The tools this adapter exports and the operation each one is.
    exports: fn() -> &'static [dispatch::Export],
    /// Old set names this adapter answers for, as `(old, current)`. The one
    /// place a superseded name is written: it marks the old name deprecated in
    /// the registry view and resolves it to the current set's grants. `old` may
    /// be a name this adapter still publishes, one it renamed away from, or one
    /// nothing publishes any more.
    superseded: &'static [(&'static str, &'static str)],
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

/// The set that supersedes `name`, if some adapter declares one.
fn canonical_capability_successor(name: &str) -> Option<&'static str> {
    adapters().find_map(|adapter| {
        adapter
            .superseded
            .iter()
            .find(|(before, _)| *before == name)
            .map(|(_, after)| *after)
    })
}

/// The registry grants a capability set confers.
///
/// A set is a name for some of an adapter's exports, so the grants it carries
/// are the grants those operations declare. Superseded `mcp/*@v1` names need
/// no translation table of their own: they name sets like any other, and a set
/// resolves to grants the same way whatever it is called.
fn capability_set_grants(name: &str) -> Vec<&'static str> {
    let name = canonical_capability_successor(name).unwrap_or(name);
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

/// Build the scoped REST client shared by Loom's resource adapters.
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

/// Shared JSON-RPC stdio loop for the resource adapters. Domain modules own
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
    /// The digests pin what each capability advertises to an agent: they move
    /// when a tool's schema changes, so a session holding e.g.
    /// `loom/artifacts/write@v1` can tell that `write`'s tool definition
    /// changed under it. Re-pin only with a reason, and check whether tool
    /// *membership* moved too.
    fn builtin_capability_digests_are_stable() {
        let expected = [
            (
                "loom/github/comment@v1",
                "sha256:5566e4a295797b020dc93d4849168deca8470a43dd5186c0e0579902d5a99da4",
            ),
            (
                "mcp/github/comment@v1",
                "sha256:a3e83e7c3a3bc69cdabda0540b5fc31c0398ca24d41f310a225ba1e1117af1de",
            ),
            (
                "loom/context/read@v1",
                "sha256:43b519dcce558f12d6d10afc0a9121fadecd888a61a19c93147ec07644893dc9",
            ),
            (
                "mcp/context/read@v1",
                "sha256:0a4807a5a163ebdbd66d52ca088150853ed3e005cc88e94ea7e58e4bfd7927ee",
            ),
            (
                "loom/channels/read@v1",
                "sha256:d90f7358facdecea9a10fd2cff6f75ccec35a76b5d0dcb08e62ca0a71b35fa2b",
            ),
            (
                "loom/channels/write@v1",
                "sha256:95e0a083ba39bffb11d7157e3b8e11aab347e66b524f9cd86ac4db83cad65973",
            ),
            (
                "mcp/channel/read@v1",
                "sha256:4895ffaa5ba9f2f7d45b4ee66501d8c20f1770f861cf81fc4463c9686050725c",
            ),
            (
                "mcp/channel/write@v1",
                "sha256:8a018f5a160e9c06d94fcc24e0b5e732ef805076d859304acf5f05e7ccaaf10c",
            ),
            (
                "loom/artifacts/read@v1",
                "sha256:2a5902dfa1a0e095c325d378cb99db76a916adf9bb485575d435afa8998c0279",
            ),
            (
                "loom/artifacts/write@v1",
                "sha256:eb99a64c4ed0e1c0c6e9dd8205f7491c06871349c857563a78f1de83fc2cd5ab",
            ),
            (
                "mcp/artifact/read@v1",
                "sha256:1ee173fad1b1b4474e0f12715dfc65107d60bdd1a7452f3da1b3ae0c0374eb05",
            ),
            (
                "mcp/artifact/write@v1",
                "sha256:6c1bba5084c89410121584af895eb0bebe686ed0d6ee6d47ed578014fc504d9a",
            ),
            (
                "loom/issues/read@v1",
                "sha256:8584e4af702fa12f19a66987f59d140ca1878ad409fdc7b68e11d0041a70f2c9",
            ),
            (
                "loom/issues/write@v1",
                "sha256:302b931d0d3eb704983d5b5be6849dfc30628e0e6b212978746a53fc75af2a5f",
            ),
            (
                "loom/sessions/read@v1",
                "sha256:f4b1830a82aa1e4e38bda28f432c666cd2b059a617ff3ae575408874b178e637",
            ),
            (
                "loom/sessions/write@v1",
                "sha256:2f26b50b697d9ae0e7a02c577ab5e5f90ead070016e59c78b228b7440442d589",
            ),
            (
                "mcp/session/read@v1",
                "sha256:5a088807c06cc31219544e04bee79bbd7ae57d3200fa54f2d9cce5d69e40c318",
            ),
            (
                "mcp/session/status@v1",
                "sha256:e9a555691d74aedd50894e7e1726ddbe0f778eb0b1b955a1b45f0d3f2b028067",
            ),
            (
                "mcp/history/self@v1",
                "sha256:57c6f4d3e48b93266ddf1ecd5c2ee60f68dd0c268a0682dba93a84fa4f57c0f4",
            ),
            (
                "mcp/messaging/status@v1",
                "sha256:1b133a74973b3b4b24efbae183cf55c59fb8d7e2010d044f3d955a0e663252cf",
            ),
            (
                "mcp/slack/message@v1",
                "sha256:e141c4a828f3d5bab35526b8cfcf54e61fcb89eeb3f99f65590a1e90ca707e3a",
            ),
            (
                "loom/permissions/read@v1",
                "sha256:0ef454860ed512f07e563baca49c8941006e76e684e42b081058587ed7210d1b",
            ),
            (
                "loom/permissions/request@v1",
                "sha256:465e7053731ceb1b047001d79751825f83518a6fb60cdc65ec2dd0502acdc83d",
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
