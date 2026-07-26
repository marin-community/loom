use axum::{
    extract::{Path, State},
    Json,
};
use std::path::Path as FsPath;
use weaver_api::ChangeSetDto;

use super::{require_session, ApiResult, AppState};

pub(super) async fn get_session_changes(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<ChangeSetDto>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(
        crate::changes::load(FsPath::new(&session.work_dir), &branch.base_branch).await?,
    ))
}
