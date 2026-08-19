use weaver_api::operations::repos::env as ops;
use weaver_api::{RepoEnvVarView, RepoEnvView};

use crate::agent_env;
use crate::db::Db;
use crate::repo_env;

use super::issues::resolve_repo_root;
use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError};

// ---------------------------------------------------------------------------
// Per-repo environment variables
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Operation registry — `repos.env.*`, bound onto
// `weaver_api::operations::repos::env`. Folded into `repos::bound_operations()`
// (see `repos.rs`) since `repos.env.*` is part of the `repos` bundle even
// though its handlers live in this sibling file.
// ---------------------------------------------------------------------------

async fn repo_env_view(db: &Db, repo_root: &str) -> ApiResult<RepoEnvView> {
    Ok(RepoEnvView {
        repo_root: repo_root.to_string(),
        env: repo_env::list(db, repo_root)
            .await?
            .into_iter()
            .map(|v| RepoEnvVarView {
                name: v.name,
                updated_at: v.updated_at,
            })
            .collect(),
    })
}

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<ops::get::Get, _, _>(env_get_operation),
        register::<ops::set::Set, _, _>(env_set_operation),
        register::<ops::delete::Delete, _, _>(env_delete_operation),
    ]
}

/// `repos.env.get`.
async fn env_get_operation(
    context: OperationContext,
    input: ops::get::Input,
) -> ApiResult<ops::get::Output> {
    let st = context.state;
    let repo_root = resolve_repo_root(input.repo_root.as_deref(), input.cwd.as_deref()).await?;
    repo_env_view(&st.db, &repo_root).await
}

/// `repos.env.set`.
async fn env_set_operation(
    context: OperationContext,
    input: ops::set::Input,
) -> ApiResult<ops::set::Output> {
    let st = context.state;
    if let Err(why) = agent_env::validate_name(&input.name) {
        return Err(AppError::bad_request(why));
    }
    let repo_root = resolve_repo_root(input.repo_root.as_deref(), input.cwd.as_deref()).await?;
    repo_env::set(&st.db, &repo_root, &input.name, &input.value).await?;
    repo_env_view(&st.db, &repo_root).await
}

/// `repos.env.delete`.
async fn env_delete_operation(
    context: OperationContext,
    input: ops::delete::Input,
) -> ApiResult<ops::delete::Output> {
    let st = context.state;
    let repo_root = resolve_repo_root(input.repo_root.as_deref(), input.cwd.as_deref()).await?;
    repo_env::remove(&st.db, &repo_root, &input.name).await?;
    repo_env_view(&st.db, &repo_root).await
}
