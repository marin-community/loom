use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{self, KeepAlive, Sse},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use weaver_api::{
    CreateSessionGroupReq, CreateSessionSpaceReq, DeleteSessionGroupReq, DeleteSessionSpaceReq,
    MoveSessionsReq, ReorderSessionLayoutReq, RestoreSessionGroupsReq, SessionGroupPreferenceReq,
    SessionLayoutView, SessionPlacementSelectorKind, SetSessionPlacementDefaultReq,
    UpdateSessionGroupReq, UpdateSessionSpaceReq,
};

use crate::auth::Principal;
use crate::session_layout::{self, MutationError};

use super::{ApiResult, AppError, AppState};

fn require_admin(principal: &Principal) -> ApiResult<()> {
    if principal.is_admin() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "shared session layout requires an admin credential",
        ))
    }
}

async fn mutation_response(
    st: &AppState,
    username: &str,
    result: Result<SessionLayoutView, MutationError>,
) -> ApiResult<Json<SessionLayoutView>> {
    match result {
        Ok(layout) => {
            session_layout::publish_invalidation(&st.db, &st.bus, layout.revision).await;
            Ok(Json(layout))
        }
        Err(MutationError::Conflict) => {
            let layout = session_layout::get_layout(&st.db, username).await?;
            Err(AppError::conflict("session layout revision changed")
                .with_fields(json!({ "layout": layout })))
        }
        Err(MutationError::Invalid(message)) => Err(AppError::bad_request(message)),
        Err(MutationError::Internal(error)) => Err(error.into()),
    }
}

pub(super) async fn get_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    Ok(Json(
        session_layout::get_layout(&st.db, &principal.username).await?,
    ))
}

pub(super) async fn create_session_space(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreateSessionSpaceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::create_space(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn update_session_space(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionSpaceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::update_space(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn delete_session_space(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<DeleteSessionSpaceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::delete_space(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn create_session_group(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreateSessionGroupReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::create_group(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn update_session_group(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionGroupReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::update_group(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn delete_session_group(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<DeleteSessionGroupReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::delete_group(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn reorder_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<ReorderSessionLayoutReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::reorder(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn move_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<MoveSessionsReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::move_sessions(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn restore_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<RestoreSessionGroupsReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::restore_groups(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn set_session_group_preference(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<SessionGroupPreferenceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    session_layout::set_preference(&st.db, &principal.username, &id, req.collapsed)
        .await
        .map(Json)
        .map_err(|error| AppError::bad_request(error.to_string()))
}

pub(super) async fn set_session_placement_default(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SetSessionPlacementDefaultReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::set_default(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteDefaultQuery {
    expected_revision: i64,
}

pub(super) async fn delete_session_placement_default(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((kind, value)): Path<(SessionPlacementSelectorKind, String)>,
    Query(query): Query<DeleteDefaultQuery>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_admin(&principal)?;
    let result = session_layout::delete_default(
        &st.db,
        &principal.username,
        kind,
        &value,
        query.expected_revision,
    )
    .await;
    mutation_response(&st, &principal.username, result).await
}

/// Fleet-global layout tail. The browser still performs a normal GET after an
/// event; the event is only invalidation, so reconnects and dropped messages
/// cannot corrupt local selection/disclosure state.
pub(super) async fn session_layout_events(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    require_admin(&principal)?;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(|result| {
        let event = result.ok()?;
        if event.kind != "session_layout" {
            return None;
        }
        Some(Ok(sse::Event::default()
            .event("session_layout")
            .json_data(&event.data)
            .unwrap_or_default()))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
