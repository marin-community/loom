//! REST boundary for canonical session-launch preview.

use axum::{extract::State, Json};
use weaver_api::{LaunchSelection, ResolveLaunchReq, ResolvedLaunchView};

use super::{ApiResult, AppError, AppState};

pub(crate) async fn resolve_launch(
    st: &AppState,
    selection: &LaunchSelection,
    options: &crate::launch::ResolveOptions,
) -> ApiResult<crate::launch::ResolvedLaunch> {
    crate::launch::resolve(&st.db, selection, options)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))
}

pub(super) async fn resolve_session_launch(
    State(st): State<AppState>,
    Json(req): Json<ResolveLaunchReq>,
) -> ApiResult<Json<ResolvedLaunchView>> {
    let profile_name = match req.selection.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    };
    let _profile_permit = st.launch_gate.acquire_profile(profile_name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    Ok(Json(
        resolve_launch(
            &st,
            &req.selection,
            &crate::launch::ResolveOptions::default(),
        )
        .await?
        .view,
    ))
}
