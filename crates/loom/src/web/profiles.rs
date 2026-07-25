use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use weaver_api::{
    CloneProfileReq, EffectiveProfileView, LaunchSelection, McpServerProcessView, ProfileEnvView,
    ProfileProbeView, ProfileReq, ProfileView, PutProfileEnvReq,
};

use crate::profile::{self, Profile, ProfileInput};

use super::{ApiResult, AppError, AppState};

pub(super) fn input(req: ProfileReq, name: String) -> ProfileInput {
    ProfileInput {
        name,
        description: req.description,
        agent_kind: req.agent_kind,
        model: req.model,
        effort: req.effort,
        protocol: req.protocol,
        mode: req.mode,
        class: req.class,
        strict: req.strict,
        env_clear: req.env_clear,
        ambient_allowlist: req.ambient_allowlist,
        idle_archive_secs: req.idle_archive_secs,
        max_concurrent: req.max_concurrent,
        turn_budget: req.turn_budget,
        prelude: req.prelude,
        restricted: req.restricted,
        allowed_tools: req.runtime_permissions,
        mcp_access: req.mcp_access,
    }
}

pub(super) async fn view(st: &AppState, profile: Profile) -> ApiResult<ProfileView> {
    let ambient_allowlist = profile
        .ambient_names()
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let env = profile::env_meta(&st.db, &profile.name)
        .await?
        .into_iter()
        .map(|entry| ProfileEnvView {
            name: entry.name,
            source: entry.source,
            secret_ref: entry.secret_ref,
            updated_at: entry.updated_at,
        })
        .collect();
    let runtime_permissions = profile
        .allowed_tool_rules()
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mcp_access = profile
        .mcp_access()
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(ProfileView {
        name: profile.name,
        description: profile.description,
        agent_kind: profile.agent_kind,
        model: profile.model,
        effort: profile.effort,
        protocol: profile.protocol,
        mode: profile.mode,
        class: profile.class,
        strict: profile.strict,
        env_clear: profile.env_clear,
        ambient_allowlist,
        idle_archive_secs: profile.idle_archive_secs,
        max_concurrent: profile.max_concurrent,
        turn_budget: profile.turn_budget,
        prelude: profile.prelude,
        restricted: profile.restricted,
        runtime_permissions,
        mcp_access,
        lifetime: profile.lifetime,
        revision: profile.revision,
        created_at: profile.created_at,
        updated_at: profile.updated_at,
        env,
    })
}

pub(super) async fn list_profiles(State(st): State<AppState>) -> ApiResult<Json<Vec<ProfileView>>> {
    let mut views = Vec::new();
    for item in profile::list(&st.db).await? {
        views.push(view(&st, item).await?);
    }
    Ok(Json(views))
}

pub(super) async fn get_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<ProfileView>> {
    let item = profile::get(&st.db, &name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    Ok(Json(view(&st, item).await?))
}

async fn effective(st: &AppState, item: Profile) -> ApiResult<EffectiveProfileView> {
    let mcp_policy = item
        .mcp_policy_snapshot()
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let runtime_permissions = item
        .effective_allowed_tool_rules_for(&mcp_policy)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let mcp_servers = crate::mcp::acp_server_configs(&runtime_permissions, Some(&mcp_policy))
        .into_iter()
        .map(|config| McpServerProcessView {
            name: config["name"].as_str().unwrap_or_default().to_string(),
            command: config["command"].as_str().unwrap_or_default().to_string(),
            args: config["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .filter_map(|arg| arg.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    Ok(EffectiveProfileView {
        profile: view(st, item).await?,
        mcp_policy,
        runtime_permissions,
        mcp_servers,
    })
}

pub(super) async fn effective_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<EffectiveProfileView>> {
    let item = profile::get(&st.db, &name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    Ok(Json(effective(&st, item).await?))
}

pub(super) async fn probe_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<ProfileProbeView>> {
    let item = profile::get(&st.db, &name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    let effective = effective(&st, item).await?;
    let errors = crate::mcp::snapshot_errors(&st.db, &effective.mcp_policy).await?;
    Ok(Json(ProfileProbeView {
        ok: errors.is_empty(),
        effective,
        errors,
    }))
}

pub(super) async fn create_profile(
    State(st): State<AppState>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<(StatusCode, Json<ProfileView>)> {
    let name = req.name.trim().to_string();
    let _permit = st.launch_gate.acquire_profile(&name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    match profile::create(&st.db, &input(req, name.clone()))
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?
    {
        profile::CreateProfileOutcome::Created(item) => {
            Ok((StatusCode::CREATED, Json(view(&st, item).await?)))
        }
        profile::CreateProfileOutcome::Exists(_) => Err(AppError::new(
            StatusCode::CONFLICT,
            format!("profile '{name}' already exists"),
        )),
    }
}

pub(super) async fn put_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<Json<ProfileView>> {
    let _permit = st.launch_gate.acquire_profile(&name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    if let Some(expected) = req.expected_revision {
        return match profile::update_expected(&st.db, &input(req, name.clone()), expected)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?
        {
            profile::UpdateProfileOutcome::Updated(item) => Ok(Json(view(&st, item).await?)),
            profile::UpdateProfileOutcome::Stale(current) => {
                let current = view(&st, current).await?;
                Err(AppError::conflict(format!(
                    "profile '{name}' changed from revision {expected} to revision {}",
                    current.revision
                ))
                .with_fields(serde_json::json!({ "profile": current })))
            }
            profile::UpdateProfileOutcome::Missing => Err(AppError::not_found("profile")),
        };
    }
    let item = profile::upsert(&st.db, &input(req, name))
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(view(&st, item).await?))
}

pub(super) async fn clone_profile(
    State(st): State<AppState>,
    Path(source_name): Path<String>,
    Json(req): Json<CloneProfileReq>,
) -> ApiResult<(StatusCode, Json<ProfileView>)> {
    let target_name = req.name.trim().to_string();
    let _permits = st
        .launch_gate
        .acquire_profiles([source_name.as_str(), target_name.as_str()])
        .await;
    // The accepted resolver fingerprint, editable normalization, and atomic
    // insert must observe one custom-agent/custom-MCP registry generation.
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    let source = profile::get(&st.db, &source_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    if source.revision != req.expected_profile_revision {
        let current = view(&st, source).await?;
        let fresh = super::launches::resolve_launch(
            &st,
            &LaunchSelection {
                profile: source_name.clone(),
                overrides: req.overrides.clone(),
            },
            &crate::launch::ResolveOptions {
                ignore_capacity: true,
                ..Default::default()
            },
        )
        .await?;
        return Err(AppError::conflict(format!(
            "profile '{source_name}' changed from revision {} to revision {}",
            req.expected_profile_revision, current.revision
        ))
        .with_fields(serde_json::json!({
            "profile": current,
            "preview": fresh.view
        })));
    }
    profile::validate_name(&target_name).map_err(AppError::bad_request)?;
    if profile::get(&st.db, &target_name).await?.is_some() {
        return Err(AppError::conflict(format!(
            "profile '{target_name}' already exists"
        )));
    }

    // Resolve the override fields against the exact source revision, but build
    // the new profile from the server-owned source input. The browser never
    // round-trips environment values, custom MCP source, or another redacted
    // policy representation.
    let resolved = match super::launches::resolve_launch(
        &st,
        &LaunchSelection {
            profile: source_name.clone(),
            overrides: req.overrides.clone(),
        },
        &crate::launch::ResolveOptions {
            ignore_capacity: true,
            ..Default::default()
        },
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => {
            return Err(AppError::conflict(format!(
                "profile resolver can no longer reproduce the accepted preview: {}",
                error.message()
            )));
        }
    };
    if resolved.view.resolver_revision != req.expected_resolver_revision {
        return Err(AppError::conflict(
            "profile resolver changed after preview; review the fresh resolution",
        )
        .with_fields(serde_json::json!({ "preview": resolved.view })));
    }
    if !resolved.view.valid {
        return Err(
            AppError::bad_request("proposed profile settings are not valid")
                .with_fields(serde_json::json!({ "preview": resolved.view })),
        );
    }
    let has_template = req.template.is_some();
    let mut cloned = match req.template {
        Some(template) => input(template, target_name.clone()),
        None => source
            .as_input()
            .map_err(|error| AppError::bad_request(error.to_string()))?,
    };
    cloned.name = target_name.clone();
    if !has_template {
        cloned.description = if source.description.trim().is_empty() {
            format!("Copy of {source_name}")
        } else {
            source.description.clone()
        };
        cloned.agent_kind = resolved.view.agent;
        cloned.model = resolved.view.model;
        cloned.effort = resolved.view.effort;
        cloned.protocol = resolved.view.protocol;
        cloned.mode = resolved.view.mode;
        cloned.class = resolved.view.class;
    }
    let prepared = profile::prepare_input(&st.db, &cloned)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let environment = req
        .environment
        .unwrap_or_else(|| weaver_api::CloneProfileEnvironmentReq {
            inherit: req.copy_environment,
            ..Default::default()
        });
    match profile::create_clone_prepared(
        &st.db,
        &source_name,
        req.expected_profile_revision,
        prepared,
        &environment,
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?
    {
        profile::CloneProfileOutcome::Created(item) => {
            Ok((StatusCode::CREATED, Json(view(&st, item).await?)))
        }
        profile::CloneProfileOutcome::Stale(current) => {
            let current = view(&st, current).await?;
            let fresh = super::launches::resolve_launch(
                &st,
                &LaunchSelection {
                    profile: source_name.clone(),
                    overrides: req.overrides,
                },
                &crate::launch::ResolveOptions {
                    ignore_capacity: true,
                    ..Default::default()
                },
            )
            .await?;
            Err(AppError::conflict(format!(
                "profile '{source_name}' changed from revision {} to revision {}",
                req.expected_profile_revision, current.revision
            ))
            .with_fields(serde_json::json!({
                "profile": current,
                "preview": fresh.view
            })))
        }
        profile::CloneProfileOutcome::TargetExists => Err(AppError::conflict(format!(
            "profile '{target_name}' already exists"
        ))),
    }
}

pub(super) async fn delete_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let _permit = st.launch_gate.acquire_profile(&name).await;
    match profile::remove(&st.db, &name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(AppError::not_found("profile")),
        Err(e) => Err(AppError::bad_request(e.to_string())),
    }
}

pub(super) async fn put_profile_env(
    State(st): State<AppState>,
    Path((profile_name, name)): Path<(String, String)>,
    Json(req): Json<PutProfileEnvReq>,
) -> ApiResult<Json<ProfileView>> {
    let _permit = st.launch_gate.acquire_profile(&profile_name).await;
    match (req.value.as_deref(), req.secret_ref.as_deref()) {
        (Some(value), None) => profile::env_set(&st.db, &profile_name, &name, value).await,
        (None, Some(secret_ref)) => {
            profile::env_set_secret(&st.db, &profile_name, &name, secret_ref).await
        }
        _ => Err(anyhow::anyhow!(
            "exactly one of value and secret_ref is required"
        )),
    }
    .map_err(|e| AppError::bad_request(e.to_string()))?;
    let item = profile::get(&st.db, &profile_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    Ok(Json(view(&st, item).await?))
}

pub(super) async fn delete_profile_env(
    State(st): State<AppState>,
    Path((profile_name, name)): Path<(String, String)>,
) -> ApiResult<Json<ProfileView>> {
    let _permit = st.launch_gate.acquire_profile(&profile_name).await;
    profile::get(&st.db, &profile_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    profile::env_remove(&st.db, &profile_name, &name).await?;
    let item = profile::get(&st.db, &profile_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    Ok(Json(view(&st, item).await?))
}
