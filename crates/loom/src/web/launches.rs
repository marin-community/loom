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
