use std::collections::BTreeSet;

use weaver_api::operations::deployment::reconcile;
use weaver_api::{DeploymentReq, DeploymentView};

use crate::config;

use super::operations::{register, Bound, OperationContext};
use super::{profiles, ApiResult, AppError, AppState};

/// Reconcile the runtime resources declared by a deployment stack. This is the
/// API-first boundary Pulumi's startup generation calls through the local Loom
/// CLI; the manifest contains references and policy, never secret values.
async fn reconcile_deployment_core(st: &AppState, req: DeploymentReq) -> ApiResult<DeploymentView> {
    let mut setting_values = Vec::with_capacity(req.settings.len());
    for (key, declared) in &req.settings {
        if config::spec(key).is_none() {
            return Err(AppError::bad_request(format!("unknown setting '{key}'")));
        }
        let value = declared.stored();
        if let Err(why) = config::validate(key, &value) {
            return Err(AppError::bad_request(format!("{key}: {why}")));
        }
        setting_values.push((key.clone(), value));
    }

    let mut profile_names = BTreeSet::new();
    for declared in &req.profiles {
        let name = declared.profile.name.trim();
        if !profile_names.insert(name.to_string()) {
            return Err(AppError::bad_request(format!(
                "profile '{name}' is declared more than once"
            )));
        }
        let mut env_names = BTreeSet::new();
        for env in &declared.env {
            if !env_names.insert(env.name.trim().to_string()) {
                return Err(AppError::bad_request(format!(
                    "profile '{name}' environment variable '{}' is declared more than once",
                    env.name
                )));
            }
        }
    }
    let mut federation_names = BTreeSet::new();
    for mapping in &req.federations {
        if !federation_names.insert(mapping.name.trim().to_string()) {
            return Err(AppError::bad_request(format!(
                "federation '{}' is declared more than once",
                mapping.name
            )));
        }
    }

    // Deployment reconciliation is another profile editor. Serialize every
    // declared lifetime plus any deployment-owned lifetime it may prune with
    // the same permit used by launch/create/handoff.
    let mut serialized_profile_names = profile_names.clone();
    if req.prune {
        for name in crate::profile::deployment_managed_names(&st.db).await? {
            serialized_profile_names.insert(name);
        }
    }
    let _profile_permits = st
        .launch_gate
        .acquire_profiles(serialized_profile_names.iter().map(String::as_str))
        .await;
    // Keep the common lock order (profiles, then registry generation). Profile
    // validation and persistence must observe one custom-agent/MCP generation.
    let _resolver_permit = st.launch_gate.acquire_resolver().await;

    config::reconcile_deployment(&st.db, &setting_values, req.prune).await?;

    for declared in &req.profiles {
        let input = super::profiles::input(
            declared.profile.clone(),
            declared.profile.name.trim().to_string(),
        );
        let profile = crate::profile::upsert(&st.db, &input)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?;
        crate::profile::mark_deployment_managed(&st.db, &profile.name).await?;

        let existing = crate::profile::env_meta(&st.db, &profile.name).await?;
        let declared_names: BTreeSet<&str> =
            declared.env.iter().map(|entry| entry.name.trim()).collect();
        for env in &declared.env {
            let name = env.name.trim();
            let result = match (env.value.as_deref(), env.secret_ref.as_deref()) {
                (Some(value), None) => {
                    crate::profile::env_set(&st.db, &profile.name, name, value).await
                }
                (None, Some(secret_ref)) => {
                    crate::profile::env_set_secret(&st.db, &profile.name, name, secret_ref).await
                }
                (None, None) if existing.iter().any(|current| current.name == name) => Ok(()),
                (None, None) => Err(anyhow::anyhow!(
                    "profile '{}' has no existing write-only value for '{name}'",
                    profile.name
                )),
                (Some(_), Some(_)) => Err(anyhow::anyhow!(
                    "profile '{}' environment '{name}' sets both value and secret_ref",
                    profile.name
                )),
            };
            result.map_err(|error| AppError::bad_request(error.to_string()))?;
        }
        for current in existing {
            if !declared_names.contains(current.name.as_str()) {
                crate::profile::env_remove(&st.db, &profile.name, &current.name).await?;
            }
        }
    }

    for mapping in &req.federations {
        crate::automation::federation_add(&st.db, mapping)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?;
        crate::automation::federation_mark_deployment_managed(&st.db, &mapping.name).await?;
    }

    if req.prune {
        for name in crate::automation::deployment_managed_federation_names(&st.db).await? {
            if !federation_names.contains(&name) {
                crate::automation::federation_remove(&st.db, &name).await?;
            }
        }
        for name in crate::profile::deployment_managed_names(&st.db).await? {
            if !profile_names.contains(&name) {
                crate::profile::remove(&st.db, &name)
                    .await
                    .map_err(|error| AppError::bad_request(error.to_string()))?;
            }
        }
    }

    let mut profile_views = Vec::new();
    for name in profile_names {
        let profile = crate::profile::get(&st.db, &name)
            .await?
            .ok_or_else(|| AppError::not_found("profile"))?;
        profile_views.push(profiles::view(st, profile).await?);
    }
    let mappings = crate::automation::federation_list(&st.db)
        .await?
        .into_iter()
        .filter(|mapping| federation_names.contains(&mapping.name))
        .collect();
    let setting_names: BTreeSet<&str> = req.settings.keys().map(String::as_str).collect();
    let settings = config::describe(&st.db)
        .await?
        .into_iter()
        .filter(|setting| setting_names.contains(setting.spec.key))
        .map(Into::into)
        .collect();
    Ok(DeploymentView {
        settings,
        profiles: profile_views,
        federations: mappings,
    })
}

/// `deployment.reconcile`. Authorized by `actor = Admin` in the central dispatcher.
pub(super) async fn reconcile_deployment_operation(
    context: OperationContext,
    input: reconcile::Input,
) -> ApiResult<DeploymentView> {
    let req = DeploymentReq {
        settings: input.settings,
        profiles: input.profiles,
        federations: input.federations,
        prune: input.prune,
    };
    reconcile_deployment_core(&context.state, req).await
}

// ---------------------------------------------------------------------------
// Operation registry — `deployment.*`, bound onto
// `weaver_api::operations::deployment`.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<reconcile::Reconcile, _, _>(
        reconcile_deployment_operation,
    )]
}
