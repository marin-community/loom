use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};

use crate::agent_env;
use crate::db::Db;

use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Operator-managed agent environment variables
// ---------------------------------------------------------------------------

async fn env_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "env": agent_env::list(db).await? })))
}

pub(super) async fn get_env(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    env_envelope(&st.db).await
}

#[derive(serde::Deserialize)]
pub(super) struct PutEnvBody {
    value: String,
}

/// Upsert one variable. The name comes from the path; the body carries the
/// value. The name is validated as a shell identifier so it can't corrupt the
/// launch script that exports it; the value is free-form.
pub(super) async fn put_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PutEnvBody>,
) -> ApiResult<Json<Value>> {
    if let Err(why) = agent_env::validate_name(&name) {
        return Err(AppError::bad_request(why));
    }
    agent_env::set(&st.db, &name, &body.value).await?;
    env_envelope(&st.db).await
}

/// Delete one variable. Returns the refreshed list; a missing name is not an
/// error (the desired end state — absent — already holds).
pub(super) async fn delete_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    agent_env::remove(&st.db, &name).await?;
    env_envelope(&st.db).await
}
