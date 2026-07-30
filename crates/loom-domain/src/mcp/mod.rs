//! Registry for Loom's built-in, restricted MCP adapters.
//!
//! Profiles select reviewed capability sets such as `mcp/github/comment`.
//! Loom expands those names into exact Claude permission rules when it stamps a
//! session, then derives the required adapter processes from that immutable
//! policy. Repositories can choose sets, but cannot inject executable adapter
//! configuration.

use anyhow::{bail, Result};
use base64::Engine as _;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, future::Future, pin::Pin};
use weaver_api::{McpAdapterView, McpCapabilitySetView, McpRegistryView};

pub mod github;
pub(crate) mod history;
pub(crate) mod messaging;

type ServeFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

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

const ADAPTERS: &[Adapter] = &[github::ADAPTER, history::ADAPTER, messaging::ADAPTER];
pub(crate) const ALLOWED_TOOLS_ENV: &str = "LOOM_MCP_ALLOWED_TOOLS";

pub(crate) fn is_tool_set(name: &str) -> bool {
    ADAPTERS
        .iter()
        .any(|adapter| (adapter.expand_tool_set)(name).is_some())
}

pub(crate) fn is_builtin_group(group: &str) -> bool {
    ADAPTERS.iter().any(|adapter| {
        (adapter.capability_sets)()
            .iter()
            .any(|set| set.group == group)
    })
}

pub fn registry() -> McpRegistryView {
    let mut adapters = Vec::new();
    let mut capability_sets = Vec::new();
    for adapter in ADAPTERS {
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
        adapters.push(McpAdapterView {
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
            });
        }
    }
    McpRegistryView {
        adapters,
        capability_sets,
        custom_servers: Vec::new(),
    }
}

/// Report whether an exact profile snapshot is still launchable. Profiles
/// remain inspectable when a set is retired or a custom server is disabled,
/// but a new session must not silently substitute current registry content.
pub async fn snapshot_errors(
    db: &crate::Db,
    snapshot: &weaver_api::McpPolicySnapshot,
) -> Result<Vec<String>> {
    let current = registry();
    let custom = crate::custom_mcp::list(db).await?;
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

pub(crate) async fn resolve_access(
    db: &crate::Db,
    access: &weaver_api::McpAccess,
) -> Result<weaver_api::McpPolicySnapshot> {
    let registry = registry();
    let custom = crate::custom_mcp::list(db).await?;
    let ready_custom = crate::custom_mcp::ready_snapshots(&custom);
    let capability_sets = match access.mode.as_str() {
        "none" => Vec::new(),
        "all" => registry.capability_sets,
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
                .filter(|set| access.groups.contains(&set.group))
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

pub(crate) fn rules_for_snapshot(snapshot: &weaver_api::McpPolicySnapshot) -> Result<Vec<String>> {
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
                crate::custom_mcp::permission_rule(&server.server_name, tool),
            );
        }
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
pub(crate) fn expand_tool_sets(rules: &[String]) -> Result<Vec<String>> {
    let mut expanded = Vec::new();
    for rule in rules {
        let tool_rules = ADAPTERS
            .iter()
            .find_map(|adapter| (adapter.expand_tool_set)(rule));
        if let Some(tool_rules) = tool_rules {
            for tool_rule in tool_rules {
                push_unique(&mut expanded, tool_rule);
            }
        } else if rule.starts_with("mcp/") {
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
    for adapter in ADAPTERS {
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

pub(crate) fn runtime_tool_allowed(name: &str) -> bool {
    runtime_allowed_tools().is_none_or(|allowed| allowed.contains(name))
}

pub(crate) fn runtime_tools(tools: Value) -> Value {
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
            let env = config["env"]
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
    let adapter = ADAPTERS
        .iter()
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
        assert_eq!(set.name, "mcp/github/comment@v1");
        assert!(set.digest.starts_with("sha256:"));
        assert_eq!(set.tools.len(), 6);
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
        for adapter in super::ADAPTERS {
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
