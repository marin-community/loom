use axum::{
    extract::{Path, State},
    Json,
};
use std::path::Path as FsPath;
use weaver_api::operations::sessions as ops;
use weaver_api::ChangeSetDto;

use super::operations::{register, Bound, OperationContext};
use super::{require_session, ApiResult, AppState};

/// The `sessions.changes` operation binding, folded into the `sessions`
/// bundle by [`super::sessions::bound_operations`].
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<ops::changes::Changes, _, _>(op_changes)]
}

/// `sessions.changes` — ported from [`get_session_changes`].
async fn op_changes(
    context: OperationContext,
    input: ops::changes::Input,
) -> ApiResult<ChangeSetDto> {
    let (session, branch) = require_session(&context.state.db, &input.session).await?;
    Ok(crate::changes::load(FsPath::new(&session.work_dir), &branch.base_branch).await?)
}
