use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use weaver_api::{ProfileEnvView, ProfileReq, ProfileView, PutProfileEnvReq};

use crate::profile::{self, Profile, ProfileInput};

use super::{ApiResult, AppError, AppState};

fn input(req: ProfileReq, name: String) -> ProfileInput {
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
    }
}

async fn view(st: &AppState, profile: Profile) -> ApiResult<ProfileView> {
    let ambient_allowlist = profile
        .ambient_names()
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let env = profile::env_meta(&st.db, &profile.name)
        .await?
        .into_iter()
        .map(|entry| ProfileEnvView {
            name: entry.name,
            updated_at: entry.updated_at,
        })
        .collect();
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

pub(super) async fn create_profile(
    State(st): State<AppState>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<(StatusCode, Json<ProfileView>)> {
    let name = req.name.trim().to_string();
    if profile::get(&st.db, &name).await?.is_some() {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            format!("profile '{name}' already exists"),
        ));
    }
    let item = profile::upsert(&st.db, &input(req, name))
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(view(&st, item).await?)))
}

pub(super) async fn put_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<Json<ProfileView>> {
    let item = profile::upsert(&st.db, &input(req, name))
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(view(&st, item).await?))
}

pub(super) async fn delete_profile(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
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
    profile::env_set(&st.db, &profile_name, &name, &req.value)
        .await
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
    let item = profile::get(&st.db, &profile_name)
        .await?
        .ok_or_else(|| AppError::not_found("profile"))?;
    profile::env_remove(&st.db, &profile_name, &name).await?;
    Ok(Json(view(&st, item).await?))
}
