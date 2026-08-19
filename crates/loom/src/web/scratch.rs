use std::path::PathBuf;

use axum::http::StatusCode;
use weaver_api::operations::sessions as ops;
use weaver_api::{ScratchDeleteResult, ScratchFileView, ScratchLimitsView};

use super::operations::{register, Bound, OperationContext};
use super::require_session;
use super::{ApiResult, AppError, AppState};

pub(super) fn map_scratch_error(error: crate::scratch::ScratchError) -> AppError {
    match error {
        crate::scratch::ScratchError::Invalid(message) => AppError::bad_request(message),
        crate::scratch::ScratchError::NotFound(message) => {
            AppError::new(StatusCode::NOT_FOUND, message)
        }
        crate::scratch::ScratchError::Internal(error) => error.into(),
    }
}

fn scratch_limits_view() -> ScratchLimitsView {
    ScratchLimitsView {
        max_files: crate::scratch::MAX_SCRATCH_FILES,
        max_file_bytes: crate::scratch::MAX_SCRATCH_FILE_BYTES,
        max_total_bytes: crate::scratch::MAX_SCRATCH_TOTAL_BYTES,
        max_name_bytes: crate::scratch::MAX_SCRATCH_NAME_BYTES,
    }
}

/// The `sessions.scratch.*` operation bindings, folded into the `sessions`
/// bundle by [`super::sessions::bound_operations`]. `sessions.scratch.write` is
/// not here: an `io = Upload` operation is bound in [`super::encodings`], which
/// calls [`write_scratch_bytes`] below.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<ops::scratch::limits::Limits, _, _>(op_scratch_limits),
        register::<ops::scratch::list::List, _, _>(op_scratch_list),
        register::<ops::scratch::delete::Delete, _, _>(op_scratch_delete),
    ]
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

/// `sessions.scratch.list` — ported from [`list_scratch`], which hand-rolled
/// the same two fields as anonymous JSON; the operation returns the
/// `ScratchFileView` that shape has become.
async fn op_scratch_list(
    context: OperationContext,
    input: ops::scratch::list::Input,
) -> ApiResult<Vec<ScratchFileView>> {
    let (session, _) = require_session(&context.state.db, &input.session).await?;
    let files = crate::scratch::list(PathBuf::from(&session.work_dir).as_path())
        .await
        .map_err(map_scratch_error)?;
    Ok(files
        .into_iter()
        .map(|file| ScratchFileView {
            name: file.name,
            bytes: file.bytes,
        })
        .collect())
}

/// `sessions.scratch.write` — one file, from the raw request body.
///
/// The authorization already ran in [`super::encodings`], which is why this
/// takes the state and the input rather than a full `OperationContext`: an
/// `io = Upload` operation's body is the payload, so its axum handler lives
/// beside the other non-JSON encodings and calls in here.
pub(super) async fn write_scratch_bytes(
    st: &AppState,
    input: &ops::scratch::write::Input,
    body: &[u8],
) -> ApiResult<weaver_api::dto::ScratchWriteResult> {
    let (session, _) = require_session(&st.db, &input.session).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let _permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let file = crate::scratch::upload(&work_dir, &input.name, body)
        .await
        .map_err(map_scratch_error)?;
    tracing::info!(
        session = %session.id,
        file = %file.name,
        bytes = file.bytes,
        "scratch file written"
    );
    Ok(weaver_api::dto::ScratchWriteResult {
        path: format!("scratch/{}", file.name),
        name: file.name,
        bytes: file.bytes,
    })
}

/// `sessions.scratch.delete` — ported from [`delete_scratch`], whose 204 becomes
/// a result body: the name reported back is the one the store resolved and
/// removed, not the raw operand.
async fn op_scratch_delete(
    context: OperationContext,
    input: ops::scratch::delete::Input,
) -> ApiResult<ScratchDeleteResult> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    let work_dir = PathBuf::from(&session.work_dir);
    let _permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let name = crate::scratch::delete(&work_dir, &input.name)
        .await
        .map_err(map_scratch_error)?;
    tracing::info!(session = %session.id, file = %name, "scratch file deleted");
    Ok(ScratchDeleteResult {
        name,
        deleted: true,
    })
}
