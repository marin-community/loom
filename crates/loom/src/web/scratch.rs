use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::operations::sessions as ops;
use weaver_api::ScratchLimitsView;

use super::operations::{register, Bound, OperationContext};
use super::require_session;
use super::{ApiResult, AppError, AppState};

#[derive(Debug, Deserialize)]
pub(super) struct ScratchQuery {
    name: String,
}

pub(super) fn map_scratch_error(error: crate::scratch::ScratchError) -> AppError {
    match error {
        crate::scratch::ScratchError::Invalid(message) => AppError::bad_request(message),
        crate::scratch::ScratchError::NotFound(message) => {
            AppError::new(StatusCode::NOT_FOUND, message)
        }
        crate::scratch::ScratchError::Internal(error) => error.into(),
    }
}

pub(super) async fn scratch_limits() -> Json<ScratchLimitsView> {
    Json(scratch_limits_view())
}

fn scratch_limits_view() -> ScratchLimitsView {
    ScratchLimitsView {
        max_files: crate::scratch::MAX_SCRATCH_FILES,
        max_file_bytes: crate::scratch::MAX_SCRATCH_FILE_BYTES,
        max_total_bytes: crate::scratch::MAX_SCRATCH_TOTAL_BYTES,
        max_name_bytes: crate::scratch::MAX_SCRATCH_NAME_BYTES,
    }
}

/// The `sessions.scratch.limits` operation binding, folded into the
/// `sessions` bundle by [`super::sessions::bound_operations`].
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<ops::scratch::limits::Limits, _, _>(
        op_scratch_limits,
    )]
}

/// `sessions.scratch.limits` — ported from [`scratch_limits`]. `actor = User`:
/// see the operation's doc comment for why — `grant_allows` in
/// `crates/loom/src/web/auth.rs` never lets a `Grant::Session` credential
/// reach the legacy `GET /scratch/limits` route this mirrors.
async fn op_scratch_limits(
    _context: OperationContext,
    _input: ops::scratch::limits::Input,
) -> ApiResult<ScratchLimitsView> {
    Ok(scratch_limits_view())
}

pub(super) async fn list_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let files = crate::scratch::list(PathBuf::from(&session.work_dir).as_path())
        .await
        .map_err(map_scratch_error)?;
    Ok(Json(
        files
            .into_iter()
            .map(|file| json!({ "name": file.name, "bytes": file.bytes }))
            .collect(),
    ))
}

pub(super) async fn upload_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<ScratchQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let _permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let file = crate::scratch::upload(&work_dir, &query.name, &body)
        .await
        .map_err(map_scratch_error)?;
    tracing::info!(
        session = %session.id,
        file = %file.name,
        bytes = file.bytes,
        "scratch file written"
    );
    Ok(Json(json!({
        "name": file.name,
        "bytes": file.bytes,
        "path": format!("scratch/{}", file.name),
    })))
}

pub(super) async fn delete_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<ScratchQuery>,
) -> ApiResult<StatusCode> {
    let (session, _) = require_session(&st.db, &key).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let _permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let name = crate::scratch::delete(&work_dir, &query.name)
        .await
        .map_err(map_scratch_error)?;
    tracing::info!(session = %session.id, file = %name, "scratch file deleted");
    Ok(StatusCode::NO_CONTENT)
}
