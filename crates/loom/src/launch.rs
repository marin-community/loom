//! Canonical profile-template resolution for previews, creates, clones, and
//! handoffs.
//!
//! Profiles remain mutable templates. A [`ResolvedLaunch`] is the concrete,
//! non-secret snapshot approved by a caller and stamped on a session. Keeping
//! this logic outside the web handlers prevents the SPA, CLI, and background
//! producers from growing subtly different launch rules.

use anyhow::{anyhow, bail, Result};
use sha2::{Digest as _, Sha256};
use weaver_api::{
    LaunchCapacityView, LaunchOverrides, LaunchProvenanceView, LaunchSelection, ProfileEnvView,
    ResolvedLaunchPolicyView, ResolvedLaunchView, SessionMcpPolicyView,
};

use crate::db::Db;
use crate::profile::Profile;

const RESOLVER_SCHEMA_VERSION: &str = "launch-resolver-v1";

/// Context-derived class for producers such as watches. `None` lets the profile
/// supply its class, which is the ordinary interactive launch behavior.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    pub default_class: Option<String>,
    /// A handoff keeps one live session rather than consuming another slot.
    /// Credit that session when it already belongs to the selected profile.
    pub capacity_credit_profile: Option<String>,
}

/// Exact server-side result. `view` is safe to return and persist;
/// `profile`/`mcp_policy` retain the internal data provisioning needs.
pub struct ResolvedLaunch {
    pub profile: Profile,
    pub mcp_policy: weaver_api::McpPolicySnapshot,
    pub runtime_permissions: Vec<String>,
    pub view: ResolvedLaunchView,
}

fn selected(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim)
}

fn nonempty(value: &Option<String>) -> Option<&str> {
    selected(value).filter(|value| !value.is_empty())
}

fn has_override(overrides: &LaunchOverrides) -> bool {
    [
        &overrides.agent,
        &overrides.model,
        &overrides.effort,
        &overrides.protocol,
        &overrides.mode,
        &overrides.class,
    ]
    .into_iter()
    .any(Option::is_some)
}

fn normalized_selection(selection: &LaunchSelection) -> LaunchSelection {
    let trim = |value: &Option<String>| value.as_ref().map(|value| value.trim().to_string());
    LaunchSelection {
        profile: selection.profile.trim().to_string(),
        overrides: LaunchOverrides {
            agent: trim(&selection.overrides.agent),
            model: trim(&selection.overrides.model),
            effort: trim(&selection.overrides.effort),
            protocol: trim(&selection.overrides.protocol),
            mode: trim(&selection.overrides.mode),
            class: trim(&selection.overrides.class),
        },
    }
}

async fn policy_defaults(db: &Db, class: &str) -> (i64, i64) {
    if class != "automation" {
        return (0, 0);
    }
    let idle = weaver_core::config::get(db, "automation.idle_archive_secs")
        .await
        .and_then(|value| value.parse().ok())
        .unwrap_or(weaver_core::config::DEFAULT_AUTOMATION_IDLE_ARCHIVE_SECS);
    let turns = weaver_core::config::get(db, "automation.turn_cap")
        .await
        .and_then(|value| value.parse().ok())
        .unwrap_or(weaver_core::config::DEFAULT_AUTOMATION_TURN_CAP);
    (idle, turns)
}

async fn resolver_revision(
    db: &Db,
    metadata: &crate::agent::AgentMetadata,
    policy_defaults: (i64, i64),
) -> Result<String> {
    // This is intentionally a global registry fingerprint. A custom runtime or
    // MCP definition changing forces every open launch form to re-preview, even
    // when its selected profile did not change. The hash never exposes custom
    // MCP source; only the server computes it.
    let mut mcp_registry = crate::mcp::registry();
    mcp_registry.custom_servers = crate::custom_mcp::list(db).await?;
    let custom_agents = crate::custom_agents::list(db).await?;
    let payload = serde_json::to_vec(&(
        RESOLVER_SCHEMA_VERSION,
        metadata,
        custom_agents,
        mcp_registry,
        policy_defaults,
    ))?;
    Ok(format!("sha256:{:x}", Sha256::digest(payload)))
}

/// Resolve one named profile plus permitted one-launch overrides.
pub async fn resolve(
    db: &Db,
    selection: &LaunchSelection,
    options: &ResolveOptions,
) -> Result<ResolvedLaunch> {
    let selection = normalized_selection(selection);
    let profile_name = if selection.profile.is_empty() {
        crate::profile::DEFAULT_PROFILE
    } else {
        &selection.profile
    };
    let profile = crate::profile::get(db, profile_name)
        .await?
        .ok_or_else(|| anyhow!("unknown profile '{profile_name}'"))?;
    let overrides = &selection.overrides;
    if profile.strict && has_override(overrides) {
        bail!("strict profile '{profile_name}' does not allow launch overrides");
    }

    let agent_overridden = overrides.agent.is_some();
    let agent = nonempty(&overrides.agent)
        .map(str::to_string)
        .unwrap_or_else(|| profile.agent_kind.clone());
    if overrides.agent.is_some() && agent.is_empty() {
        bail!("launch override agent must not be empty");
    }
    let metadata = crate::agent::metadata_for(db, &agent)
        .await?
        .ok_or_else(|| anyhow!("unknown agent '{agent}'"))?;

    let (model, model_source) = match selected(&overrides.model) {
        Some(value) => (value.to_string(), "launch_override"),
        None if agent_overridden => (String::new(), "agent_default"),
        None if profile.model.is_empty() => (String::new(), "agent_default"),
        None => (profile.model.clone(), "profile"),
    };
    let (effort, effort_source) = match selected(&overrides.effort) {
        Some(value) => (value.to_string(), "launch_override"),
        None if agent_overridden => (String::new(), "agent_default"),
        None if profile.effort.is_empty() => (String::new(), "agent_default"),
        None => (profile.effort.clone(), "profile"),
    };
    crate::agent::validate_model(&metadata, &model).map_err(|error| anyhow!(error))?;
    crate::agent::validate_effort(&metadata, &effort).map_err(|error| anyhow!(error))?;

    let protocol_requested = selected(&overrides.protocol).or_else(|| {
        (!agent_overridden && !profile.protocol.trim().is_empty())
            .then_some(profile.protocol.as_str())
    });
    let protocol = crate::agent::resolve_protocol(&metadata, protocol_requested)
        .map_err(|error| anyhow!(error))?;
    let protocol_source = if overrides.protocol.is_some() {
        "launch_override"
    } else if agent_overridden || profile.protocol.trim().is_empty() {
        "agent_default"
    } else {
        "profile"
    };

    let (mode, mode_source) = match nonempty(&overrides.mode) {
        Some(value) => (value.to_string(), "launch_override"),
        None => (profile.mode.clone(), "profile"),
    };
    if !matches!(
        mode.as_str(),
        "auto" | "default" | "acceptEdits" | "plan" | "bypassPermissions"
    ) {
        bail!("invalid launch mode '{mode}'");
    }

    let (class, class_source) = match nonempty(&overrides.class) {
        Some(value) => (value.to_string(), "launch_override"),
        None => match &options.default_class {
            Some(value) => (value.clone(), "origin_default"),
            None => (profile.class.clone(), "profile"),
        },
    };
    if !matches!(class.as_str(), "interactive" | "automation") {
        bail!("invalid class '{class}' (expected 'interactive' or 'automation')");
    }

    let mcp_policy = profile.mcp_policy_snapshot()?;
    let mut errors = crate::mcp::snapshot_errors(db, &mcp_policy).await?;
    let runtime_permissions = profile.effective_allowed_tool_rules_for(&mcp_policy)?;
    let active = crate::profile::active_count(db, profile_name).await?;
    let maximum = (profile.max_concurrent > 0).then_some(profile.max_concurrent);
    let available = maximum.map(|maximum| (maximum - active).max(0));
    let keeps_existing_slot = options
        .capacity_credit_profile
        .as_deref()
        .is_some_and(|current| current == profile_name);
    let allowed = keeps_existing_slot || available.is_none_or(|available| available > 0);
    if !allowed {
        errors.push(format!(
            "profile '{profile_name}' has reached its max_concurrent limit ({})",
            profile.max_concurrent
        ));
    }

    let environment = crate::profile::env_meta(db, profile_name)
        .await?
        .into_iter()
        .map(|entry| ProfileEnvView {
            name: entry.name,
            source: entry.source,
            secret_ref: entry.secret_ref,
            updated_at: entry.updated_at,
        })
        .collect();
    let defaults = policy_defaults(db, &class).await;
    let idle_archive_secs = profile.idle_archive_secs.unwrap_or(defaults.0);
    let turn_budget = profile.turn_budget.unwrap_or(defaults.1);
    let resolver_revision = resolver_revision(db, &metadata, defaults).await?;
    let locked_fields = if profile.strict {
        ["agent", "model", "effort", "protocol", "mode", "class"]
            .into_iter()
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };
    let view = ResolvedLaunchView {
        selection: LaunchSelection {
            profile: profile_name.to_string(),
            overrides: selection.overrides.clone(),
        },
        profile_revision: profile.revision,
        resolver_revision,
        agent,
        model,
        effort,
        protocol,
        mode,
        class,
        locked_fields,
        provenance: LaunchProvenanceView {
            agent: if overrides.agent.is_some() {
                "launch_override".to_string()
            } else {
                "profile".to_string()
            },
            model: model_source.to_string(),
            effort: effort_source.to_string(),
            protocol: protocol_source.to_string(),
            mode: mode_source.to_string(),
            class: class_source.to_string(),
            idle_archive_secs: if profile.idle_archive_secs.is_some() {
                "profile"
            } else {
                "policy_default"
            }
            .to_string(),
            turn_budget: if profile.turn_budget.is_some() {
                "profile"
            } else {
                "policy_default"
            }
            .to_string(),
        },
        capacity: LaunchCapacityView {
            active,
            maximum,
            available,
            allowed,
        },
        policy: ResolvedLaunchPolicyView {
            strict: profile.strict,
            restricted: profile.restricted,
            env_clear: profile.env_clear,
            environment,
            ambient_allowlist: profile.ambient_names()?,
            idle_archive_secs: Some(idle_archive_secs),
            turn_budget: Some(turn_budget),
            prelude: profile.prelude.clone(),
            runtime_permissions: runtime_permissions.clone(),
            mcp_policy: SessionMcpPolicyView::from(&mcp_policy),
        },
        valid: errors.is_empty(),
        errors,
    };
    Ok(ResolvedLaunch {
        profile,
        mcp_policy,
        runtime_permissions,
        view,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn override_resolution_tracks_provenance_without_mutating_profile() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let before = crate::profile::get(&db, "default").await.unwrap().unwrap();
        let resolved = resolve(
            &db,
            &LaunchSelection {
                profile: "default".to_string(),
                overrides: LaunchOverrides {
                    agent: Some("codex".to_string()),
                    model: Some("gpt-5.6-sol".to_string()),
                    effort: Some("high".to_string()),
                    ..Default::default()
                },
            },
            &ResolveOptions::default(),
        )
        .await
        .unwrap();

        assert_eq!(resolved.view.agent, "codex");
        assert_eq!(resolved.view.provenance.agent, "launch_override");
        assert_eq!(resolved.view.provenance.model, "launch_override");
        assert_eq!(resolved.view.provenance.effort, "launch_override");
        assert_eq!(
            crate::profile::get(&db, "default")
                .await
                .unwrap()
                .unwrap()
                .revision,
            before.revision
        );
    }

    #[tokio::test]
    async fn strict_profile_rejects_every_override() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let error = resolve(
            &db,
            &LaunchSelection {
                profile: "watch".to_string(),
                overrides: LaunchOverrides {
                    effort: Some("high".to_string()),
                    ..Default::default()
                },
            },
            &ResolveOptions::default(),
        )
        .await
        .err()
        .expect("strict override rejected");
        assert!(error
            .to_string()
            .contains("does not allow launch overrides"));
    }

    #[tokio::test]
    async fn empty_profile_selectors_report_agent_default_provenance() {
        let db = crate::db::connect_in_memory().await.unwrap();
        sqlx::query(
            "UPDATE profiles SET model = '', effort = '', protocol = '' WHERE name = 'default'",
        )
        .execute(&db)
        .await
        .unwrap();
        let resolved = resolve(&db, &LaunchSelection::default(), &ResolveOptions::default())
            .await
            .unwrap();

        assert_eq!(resolved.view.provenance.model, "agent_default");
        assert_eq!(resolved.view.provenance.effort, "agent_default");
        assert_eq!(resolved.view.provenance.protocol, "agent_default");
    }

    #[tokio::test]
    async fn policy_default_changes_advance_the_resolver_revision() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let options = ResolveOptions {
            default_class: Some("automation".to_string()),
            ..Default::default()
        };
        let before = resolve(&db, &LaunchSelection::default(), &options)
            .await
            .unwrap();
        assert_eq!(before.view.provenance.idle_archive_secs, "policy_default");

        weaver_core::config::apply(
            &db,
            &[(
                "automation.idle_archive_secs".to_string(),
                Some("1234".to_string()),
            )],
        )
        .await
        .unwrap();
        let after = resolve(&db, &LaunchSelection::default(), &options)
            .await
            .unwrap();
        assert_eq!(after.view.policy.idle_archive_secs, Some(1234));
        assert_ne!(before.view.resolver_revision, after.view.resolver_revision);
    }
}
