use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use weaver_api::operations::settings::env as settings_env_operations;
use weaver_api::AgentEnvVarView;

use crate::db::Db;
use crate::{agent_env, profile};

use super::operations::OperationContext;
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Operator-managed agent environment variables
// ---------------------------------------------------------------------------

/// The default profile's environment, as the wire [`AgentEnvVarView`] list —
/// values included in full, matching [`agent_env::list`] (see that module's
/// doc comment: this facade predates the write-only convention profiles use).
async fn env_vars(db: &Db) -> ApiResult<Vec<AgentEnvVarView>> {
    Ok(agent_env::list(db)
        .await?
        .into_iter()
        .map(|entry| AgentEnvVarView {
            name: entry.name,
            value: entry.value,
            updated_at: entry.updated_at,
        })
        .collect())
}

async fn env_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "env": env_vars(db).await? })))
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
    let _profile_permit = st
        .launch_gate
        .acquire_profile(profile::DEFAULT_PROFILE)
        .await;
    profile::env_set(&st.db, profile::DEFAULT_PROFILE, &name, &body.value).await?;
    env_envelope(&st.db).await
}

/// Delete one variable. Returns the refreshed list; a missing name is not an
/// error (the desired end state — absent — already holds).
pub(super) async fn delete_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let _profile_permit = st
        .launch_gate
        .acquire_profile(profile::DEFAULT_PROFILE)
        .await;
    profile::env_remove(&st.db, profile::DEFAULT_PROFILE, &name).await?;
    env_envelope(&st.db).await
}

// ---------------------------------------------------------------------------
// Operation registry — `settings.env.*`. Bound from `web/settings.rs`'s
// `bound_operations()` (the `settings` bundle owns the descriptors), since
// this facade is a sub-resource of `settings`, not its own bundle.
// ---------------------------------------------------------------------------

/// `settings.env.list` — the twin of [`get_env`].
pub(super) async fn list_settings_env_operation(
    context: OperationContext,
    _input: settings_env_operations::list::Input,
) -> ApiResult<Vec<AgentEnvVarView>> {
    env_vars(&context.state.db).await
}

/// `settings.env.set` — the twin of [`put_env`].
pub(super) async fn set_settings_env_operation(
    context: OperationContext,
    input: settings_env_operations::set::Input,
) -> ApiResult<Vec<AgentEnvVarView>> {
    let st = context.state;
    if let Err(why) = agent_env::validate_name(&input.name) {
        return Err(AppError::bad_request(why));
    }
    let _profile_permit = st
        .launch_gate
        .acquire_profile(profile::DEFAULT_PROFILE)
        .await;
    profile::env_set(&st.db, profile::DEFAULT_PROFILE, &input.name, &input.value).await?;
    env_vars(&st.db).await
}

/// `settings.env.delete` — the twin of [`delete_env`].
pub(super) async fn delete_settings_env_operation(
    context: OperationContext,
    input: settings_env_operations::delete::Input,
) -> ApiResult<Vec<AgentEnvVarView>> {
    let st = context.state;
    let _profile_permit = st
        .launch_gate
        .acquire_profile(profile::DEFAULT_PROFILE)
        .await;
    profile::env_remove(&st.db, profile::DEFAULT_PROFILE, &input.name).await?;
    env_vars(&st.db).await
}
