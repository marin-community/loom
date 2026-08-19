use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use weaver_api::operations::profiles as profiles_operations;
use weaver_api::{
    CloneProfileReq, EffectiveProfileView, LaunchSelection, McpServerProcessView,
    ProfileDeleteResult, ProfileEnvView, ProfileReq, ProfileView, PutProfileEnvReq,
};

use crate::profile::{self, Profile, ProfileInput};

use super::operations::{register, Bound, OperationContext};
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
        instructions: req.instructions,
        restricted: req.restricted,
        github_repositories: req.github_repositories,
        allowed_tools: req.runtime_permissions,
        mcp_access: req.mcp_access,
    }
}

/// The twin of [`input`], from `profiles.create`'s own typed fields rather
/// than the shared [`ProfileReq`] the legacy route deserializes.
fn profile_input_from_create(input: profiles_operations::create::Input, name: String) -> ProfileInput {
    ProfileInput {
        name,
        description: input.description,
        agent_kind: input.agent_kind,
        model: input.model,
        effort: input.effort,
        protocol: input.protocol,
        mode: input.mode,
        class: input.class,
        strict: input.strict,
        env_clear: input.env_clear,
        ambient_allowlist: input.ambient_allowlist,
        idle_archive_secs: input.idle_archive_secs,
        max_concurrent: input.max_concurrent,
        turn_budget: input.turn_budget,
        prelude: input.prelude,
        instructions: input.instructions,
        restricted: input.restricted,
        github_repositories: input.github_repositories,
        allowed_tools: input.runtime_permissions,
        mcp_access: input.mcp_access,
    }
}

/// The twin of [`input`], from `profiles.update`'s own typed fields.
fn profile_input_from_update(input: profiles_operations::update::Input, name: String) -> ProfileInput {
    ProfileInput {
        name,
        description: input.description,
        agent_kind: input.agent_kind,
        model: input.model,
        effort: input.effort,
        protocol: input.protocol,
        mode: input.mode,
        class: input.class,
        strict: input.strict,
        env_clear: input.env_clear,
        ambient_allowlist: input.ambient_allowlist,
        idle_archive_secs: input.idle_archive_secs,
        max_concurrent: input.max_concurrent,
        turn_budget: input.turn_budget,
        prelude: input.prelude,
        instructions: input.instructions,
        restricted: input.restricted,
        github_repositories: input.github_repositories,
        allowed_tools: input.runtime_permissions,
        mcp_access: input.mcp_access,
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
    let github_repositories = profile
        .github_repositories()
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
        instructions: profile.instructions,
        restricted: profile.restricted,
        github_repositories,
        runtime_permissions,
        mcp_access,
        lifetime: profile.lifetime,
        revision: profile.revision,
        created_at: profile.created_at,
        updated_at: profile.updated_at,
        env,
    })
}

async fn list_profiles_core(st: &AppState) -> ApiResult<Vec<ProfileView>> {
    let mut views = Vec::new();
    for item in profile::list(&st.db).await? {
        views.push(view(st, item).await?);
    }
    Ok(views)
}

pub(super) async fn list_profiles(State(st): State<AppState>) -> ApiResult<Json<Vec<ProfileView>>> {
    Ok(Json(list_profiles_core(&st).await?))
}

/// `profiles.list` — the twin of [`list_profiles`].
pub(super) async fn list_profiles_operation(
    context: OperationContext,
    _input: profiles_operations::list::Input,
) -> ApiResult<Vec<ProfileView>> {
    list_profiles_core(&context.state).await
}

async fn get_profile_core(st: &AppState, name: &str) -> ApiResult<ProfileView> {
    let item = profile::get(&st.db, name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    view(st, item).await
}

pub(super) async fn get_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<ProfileView>> {
    Ok(Json(get_profile_core(&st, &name).await?))
}

/// `profiles.get` — the twin of [`get_profile`].
pub(super) async fn get_profile_operation(
    context: OperationContext,
    input: profiles_operations::get::Input,
) -> ApiResult<ProfileView> {
    get_profile_core(&context.state, &input.name).await
}

async fn effective(st: &AppState, item: Profile) -> ApiResult<EffectiveProfileView> {
    let mcp_policy = item
        .mcp_policy_snapshot()
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let runtime_permissions = profile::effective_allowed_tool_rules_for(&item, &mcp_policy)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let mcp_servers = crate::mcp::acp_server_configs(&runtime_permissions, Some(&mcp_policy), &[])
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

async fn effective_profile_core(st: &AppState, name: &str) -> ApiResult<EffectiveProfileView> {
    let item = profile::get(&st.db, name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    effective(st, item).await
}

pub(super) async fn effective_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<EffectiveProfileView>> {
    Ok(Json(effective_profile_core(&st, &name).await?))
}

/// `profiles.effective` — the twin of [`effective_profile`].
pub(super) async fn effective_profile_operation(
    context: OperationContext,
    input: profiles_operations::effective::Input,
) -> ApiResult<EffectiveProfileView> {
    effective_profile_core(&context.state, &input.name).await
}

async fn create_profile_core(
    st: &AppState,
    name: String,
    profile_input: ProfileInput,
) -> ApiResult<ProfileView> {
    let _permit = st.launch_gate.acquire_profile(&name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    match profile::create(&st.db, &profile_input)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?
    {
        profile::CreateProfileOutcome::Created(item) => view(st, item).await,
        profile::CreateProfileOutcome::Exists(_) => Err(AppError::new(
            StatusCode::CONFLICT,
            format!("profile '{name}' already exists"),
        )),
    }
}

pub(super) async fn create_profile(
    State(st): State<AppState>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<(StatusCode, Json<ProfileView>)> {
    let name = req.name.trim().to_string();
    let created = create_profile_core(&st, name.clone(), input(req, name)).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `profiles.create` — the twin of [`create_profile`].
pub(super) async fn create_profile_operation(
    context: OperationContext,
    input: profiles_operations::create::Input,
) -> ApiResult<ProfileView> {
    let name = input.name.trim().to_string();
    create_profile_core(
        &context.state,
        name.clone(),
        profile_input_from_create(input, name),
    )
    .await
}

async fn update_profile_core(
    st: &AppState,
    name: String,
    profile_input: ProfileInput,
    expected_revision: Option<i64>,
) -> ApiResult<ProfileView> {
    let _permit = st.launch_gate.acquire_profile(&name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    if let Some(expected) = expected_revision {
        return match profile::update_expected(&st.db, &profile_input, expected)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?
        {
            profile::UpdateProfileOutcome::Updated(item) => view(st, item).await,
            profile::UpdateProfileOutcome::Stale(current) => {
                let current = view(st, current).await?;
                Err(AppError::conflict(format!(
                    "profile '{name}' changed from revision {expected} to revision {}",
                    current.revision
                ))
                .with_fields(serde_json::json!({ "profile": current })))
            }
            profile::UpdateProfileOutcome::Missing => Err(AppError::not_found("profile")),
        };
    }
    let item = profile::upsert(&st.db, &profile_input)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    view(st, item).await
}

pub(super) async fn put_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<Json<ProfileView>> {
    let expected_revision = req.expected_revision;
    Ok(Json(
        update_profile_core(&st, name.clone(), input(req, name), expected_revision).await?,
    ))
}

/// `profiles.update` — the twin of [`put_profile`].
pub(super) async fn update_profile_operation(
    context: OperationContext,
    input: profiles_operations::update::Input,
) -> ApiResult<ProfileView> {
    let name = input.name.clone();
    let expected_revision = input.expected_revision;
    update_profile_core(
        &context.state,
        name.clone(),
        profile_input_from_update(input, name),
        expected_revision,
    )
    .await
}

async fn clone_profile_core(
    st: &AppState,
    source_name: String,
    req: CloneProfileReq,
) -> ApiResult<ProfileView> {
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
        let current = view(st, source).await?;
        let fresh = super::launches::resolve_launch(
            st,
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
        st,
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
        profile::CloneProfileOutcome::Created(item) => view(st, item).await,
        profile::CloneProfileOutcome::Stale(current) => {
            let current = view(st, current).await?;
            let fresh = super::launches::resolve_launch(
                st,
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

pub(super) async fn clone_profile(
    State(st): State<AppState>,
    Path(source_name): Path<String>,
    Json(req): Json<CloneProfileReq>,
) -> ApiResult<(StatusCode, Json<ProfileView>)> {
    let cloned = clone_profile_core(&st, source_name, req).await?;
    Ok((StatusCode::CREATED, Json(cloned)))
}

/// `profiles.clone` — the twin of [`clone_profile`]. `source` and `name` are
/// separate positional fields on the operation (the legacy route split them
/// as a URL path segment plus a body field); [`CloneProfileReq`] is otherwise
/// the same frozen shape both take.
pub(super) async fn clone_profile_operation(
    context: OperationContext,
    input: profiles_operations::clone::Input,
) -> ApiResult<ProfileView> {
    let source = input.source;
    let req = CloneProfileReq {
        name: input.name,
        expected_profile_revision: input.expected_profile_revision,
        expected_resolver_revision: input.expected_resolver_revision,
        overrides: input.overrides,
        template: input.template,
        copy_environment: input.copy_environment,
        environment: input.environment,
    };
    clone_profile_core(&context.state, source, req).await
}

async fn delete_profile_core(st: &AppState, name: String) -> ApiResult<ProfileDeleteResult> {
    let _permit = st.launch_gate.acquire_profile(&name).await;
    match profile::remove(&st.db, &name).await {
        Ok(true) => Ok(ProfileDeleteResult {
            deleted: true,
            name,
        }),
        Ok(false) => Err(AppError::not_found("profile")),
        Err(e) => Err(AppError::bad_request(e.to_string())),
    }
}

pub(super) async fn delete_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    delete_profile_core(&st, name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `profiles.delete` — the twin of [`delete_profile`].
pub(super) async fn delete_profile_operation(
    context: OperationContext,
    input: profiles_operations::delete::Input,
) -> ApiResult<ProfileDeleteResult> {
    delete_profile_core(&context.state, input.name).await
}

async fn env_set_profile_core(
    st: &AppState,
    profile_name: String,
    name: String,
    value: Option<String>,
    secret_ref: Option<String>,
) -> ApiResult<ProfileView> {
    let _permit = st.launch_gate.acquire_profile(&profile_name).await;
    match (value.as_deref(), secret_ref.as_deref()) {
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
    view(st, item).await
}

pub(super) async fn put_profile_env(
    State(st): State<AppState>,
    Path((profile_name, name)): Path<(String, String)>,
    Json(req): Json<PutProfileEnvReq>,
) -> ApiResult<Json<ProfileView>> {
    Ok(Json(
        env_set_profile_core(&st, profile_name, name, req.value, req.secret_ref).await?,
    ))
}

/// `profiles.env.set` — the twin of [`put_profile_env`].
pub(super) async fn set_profile_env_operation(
    context: OperationContext,
    input: profiles_operations::env::set::Input,
) -> ApiResult<ProfileView> {
    env_set_profile_core(
        &context.state,
        input.profile,
        input.name,
        input.value,
        input.secret_ref,
    )
    .await
}

async fn env_delete_profile_core(
    st: &AppState,
    profile_name: String,
    name: String,
) -> ApiResult<ProfileView> {
    let _permit = st.launch_gate.acquire_profile(&profile_name).await;
    profile::get(&st.db, &profile_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    profile::env_remove(&st.db, &profile_name, &name).await?;
    let item = profile::get(&st.db, &profile_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    view(st, item).await
}

pub(super) async fn delete_profile_env(
    State(st): State<AppState>,
    Path((profile_name, name)): Path<(String, String)>,
) -> ApiResult<Json<ProfileView>> {
    Ok(Json(
        env_delete_profile_core(&st, profile_name, name).await?,
    ))
}

/// `profiles.env.delete` — the twin of [`delete_profile_env`].
pub(super) async fn delete_profile_env_operation(
    context: OperationContext,
    input: profiles_operations::env::delete::Input,
) -> ApiResult<ProfileView> {
    env_delete_profile_core(&context.state, input.profile, input.name).await
}

// ---------------------------------------------------------------------------
// Operation registry — `profiles.*`, bound onto
// `weaver_api::operations::profiles`.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<profiles_operations::list::List, _, _>(list_profiles_operation),
        register::<profiles_operations::get::Get, _, _>(get_profile_operation),
        register::<profiles_operations::effective::Effective, _, _>(effective_profile_operation),
        register::<profiles_operations::create::Create, _, _>(create_profile_operation),
        register::<profiles_operations::update::Update, _, _>(update_profile_operation),
        register::<profiles_operations::delete::Delete, _, _>(delete_profile_operation),
        register::<profiles_operations::clone::Clone, _, _>(clone_profile_operation),
        register::<profiles_operations::env::set::Set, _, _>(set_profile_env_operation),
        register::<profiles_operations::env::delete::Delete, _, _>(delete_profile_env_operation),
    ]
}
