use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent_env;
use crate::db::Db;
use crate::repo_env;

use super::issues::resolve_repo_root;
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Per-repo environment variables
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct RepoEnvQuery {
    /// Repo to scope to (canonical primary-worktree path), like the issue board.
    repo_root: Option<String>,
    /// Alternative for callers that only know a directory; resolved server-side.
    cwd: Option<String>,
}

async fn repo_env_envelope(db: &Db, repo_root: &str) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "repo_root": repo_root,
        "env": repo_env::list(db, repo_root).await?,
    })))
}

/// `GET /api/repos/env?repo_root=…` — the per-repo variables' *metadata* for a
/// repo. Names and timestamps only: values are write-only and never returned.
pub(super) async fn get_repo_env(
    State(st): State<AppState>,
    Query(q): Query<RepoEnvQuery>,
) -> ApiResult<Json<Value>> {
    let repo_root = resolve_repo_root(q.repo_root.as_deref(), q.cwd.as_deref()).await?;
    repo_env_envelope(&st.db, &repo_root).await
}

/// Body for `PUT /api/repos/env/{name}`: the repo to scope to plus the value.
#[derive(Debug, Deserialize)]
pub(super) struct PutRepoEnvBody {
    repo_root: Option<String>,
    cwd: Option<String>,
    value: String,
}

/// `PUT /api/repos/env/{name}` — upsert one per-repo variable. The name (from the
/// path) is validated as a shell identifier that isn't one of loom's reserved
/// `WEAVER_`/`LOOM_` names, so it can't corrupt or shadow the launch env that
/// exports it; the value is free-form and write-only. Returns the refreshed
/// metadata list (no values).
pub(super) async fn put_repo_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PutRepoEnvBody>,
) -> ApiResult<Json<Value>> {
    if let Err(why) = agent_env::validate_name(&name) {
        return Err(AppError::bad_request(why));
    }
    let repo_root = resolve_repo_root(body.repo_root.as_deref(), body.cwd.as_deref()).await?;
    repo_env::set(&st.db, &repo_root, &name, &body.value).await?;
    repo_env_envelope(&st.db, &repo_root).await
}

/// `DELETE /api/repos/env/{name}?repo_root=…` — remove one per-repo variable. A
/// missing name is not an error (the desired end state already holds).
pub(super) async fn delete_repo_env(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<RepoEnvQuery>,
) -> ApiResult<Json<Value>> {
    let repo_root = resolve_repo_root(q.repo_root.as_deref(), q.cwd.as_deref()).await?;
    repo_env::remove(&st.db, &repo_root, &name).await?;
    repo_env_envelope(&st.db, &repo_root).await
}

/// `POST /api/shell/restart` — reset the operator scratch shell, killing the
/// current supervisor and spawning a fresh one. Handy after editing operator env
/// vars (the new shell picks them up) or to clear a wedged session.
pub(super) async fn restart_shell(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    crate::shell::restart(&st).await?;
    Ok(Json(json!({ "restarted": true })))
}
