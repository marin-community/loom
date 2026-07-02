use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use weaver_api::{AnchorDto, CommentDto, NewCommentBody, NewThreadBody, ThreadDto};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch::Branch;
use weaver_core::discussion;

use crate::events;

use super::{require_branch, require_session};
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Discussion — resolvable, stand-off comment threads anchored to a quoted span
// of an artifact. See `weaver_core::discussion` and docs/artifacts.md. Name
// resolution mirrors the artifact endpoints (branch-scoped, then
// repo-shared); API-originated threads/comments are authored `"user"`. The
// branch-scoped twins below serve `weaver artifact comment/resolve/threads`,
// which — like every other `weaver` command — needs no live session.
// ---------------------------------------------------------------------------

/// Map a domain [`discussion::Thread`] to its wire [`ThreadDto`].
fn thread_dto(t: &discussion::Thread) -> ThreadDto {
    ThreadDto {
        id: t.id,
        base_rev: t.base_rev,
        anchor: AnchorDto {
            quote: t.anchor_quote.clone(),
            prefix: t.anchor_prefix.clone(),
            suffix: t.anchor_suffix.clone(),
        },
        status: t.status.clone(),
        created_at: t.created_at.clone(),
        resolved_at: t.resolved_at.clone(),
        comments: t
            .comments
            .iter()
            .map(|c| CommentDto {
                seq: c.seq,
                author: c.author.clone(),
                body: c.body.clone(),
                created_at: c.created_at.clone(),
            })
            .collect(),
    }
}

/// Resolve `{name}` to an artifact visible from the session (branch-scoped then
/// repo-shared, the way `get_artifact` does), returning the session's branch
/// alongside it. Shared by every session-scoped thread endpoint.
async fn session_artifact(st: &AppState, key: &str, name: &str) -> ApiResult<(Branch, Artifact)> {
    let (_, branch) = require_session(&st.db, key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    Ok((branch, a))
}

/// Resolve a thread by id, confirming it belongs to the named artifact the
/// session can see — so a thread id can't be probed across artifacts or
/// sessions.
async fn session_thread(
    st: &AppState,
    key: &str,
    name: &str,
    tid: i64,
) -> ApiResult<(Branch, Artifact, discussion::Thread)> {
    let (branch, a) = session_artifact(st, key, name).await?;
    let thread = discussion::get_thread(&st.db, tid)
        .await?
        .filter(|t| t.artifact_id == a.id)
        .ok_or_else(|| AppError::not_found("thread"))?;
    Ok((branch, a, thread))
}

/// `GET /sessions/{id}/artifacts/{name}/threads` — every thread on an
/// artifact, open and resolved, each with its comments.
pub(super) async fn list_threads(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<ThreadDto>>> {
    let (_, a) = session_artifact(&st, &key, &name).await?;
    let threads = discussion::list_for_artifact(&st.db, a.id, true).await?;
    Ok(Json(threads.iter().map(thread_dto).collect()))
}

/// `POST /sessions/{id}/artifacts/{name}/threads` — open a new thread anchored
/// to a quoted span, seeded with its first comment. Author is `"user"`.
pub(super) async fn create_thread(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Json(body): Json<NewThreadBody>,
) -> ApiResult<Json<ThreadDto>> {
    let (branch, a) = session_artifact(&st, &key, &name).await?;
    let thread = discussion::create_thread(
        &st.db,
        &discussion::NewThread {
            artifact_id: a.id,
            base_rev: body.base_rev,
            anchor_quote: &body.anchor.quote,
            anchor_prefix: &body.anchor.prefix,
            anchor_suffix: &body.anchor.suffix,
            author: "user",
            body: &body.body,
        },
    )
    .await?;
    tracing::info!(artifact = %name, thread = thread.id, "comment posted");
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_added",
        json!({ "artifact": name, "thread": thread.id, "seq": 1, "author": "user" }),
    )
    .await
    .ok();
    Ok(Json(thread_dto(&thread)))
}

/// `POST /sessions/{id}/artifacts/{name}/threads/{tid}/comments` — append a
/// reply to an existing thread. Author is `"user"`.
pub(super) async fn add_comment(
    State(st): State<AppState>,
    Path((key, name, tid)): Path<(String, String, i64)>,
    Json(body): Json<NewCommentBody>,
) -> ApiResult<Json<CommentDto>> {
    let (branch, _a, thread) = session_thread(&st, &key, &name, tid).await?;
    let comment = discussion::add_comment(&st.db, thread.id, "user", &body.body).await?;
    tracing::info!(artifact = %name, thread = thread.id, seq = comment.seq, "comment posted");
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_added",
        json!({ "artifact": name, "thread": thread.id, "seq": comment.seq, "author": "user" }),
    )
    .await
    .ok();
    Ok(Json(CommentDto {
        seq: comment.seq,
        author: comment.author,
        body: comment.body,
        created_at: comment.created_at,
    }))
}

/// `POST /sessions/{id}/artifacts/{name}/threads/{tid}/resolve` — mark a
/// thread resolved.
pub(super) async fn resolve_thread(
    State(st): State<AppState>,
    Path((key, name, tid)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let (branch, _a, thread) = session_thread(&st, &key, &name, tid).await?;
    discussion::resolve(&st.db, thread.id).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_resolved",
        json!({ "artifact": name, "thread": thread.id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "resolved": true })))
}

// ---------------------------------------------------------------------------
// Branch-scoped discussion — the twin of the session-scoped routes above, for
// a `weaver artifact comment/resolve/threads` target with no live session
// required, matching the branch-scoped artifact routes in `artifacts.rs`.
// ---------------------------------------------------------------------------

/// Resolve `{name}` to an artifact visible from the branch (branch-scoped then
/// repo-shared), returning the branch alongside it. Shared by every
/// branch-scoped thread endpoint.
async fn branch_artifact(st: &AppState, key: &str, name: &str) -> ApiResult<(Branch, Artifact)> {
    let branch = require_branch(&st.db, key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    Ok((branch, a))
}

/// Resolve a thread by id, confirming it belongs to the named artifact the
/// branch can see.
async fn branch_thread(
    st: &AppState,
    key: &str,
    name: &str,
    tid: i64,
) -> ApiResult<(Branch, Artifact, discussion::Thread)> {
    let (branch, a) = branch_artifact(st, key, name).await?;
    let thread = discussion::get_thread(&st.db, tid)
        .await?
        .filter(|t| t.artifact_id == a.id)
        .ok_or_else(|| AppError::not_found("thread"))?;
    Ok((branch, a, thread))
}

/// `GET /branches/{id}/artifacts/{name}/threads` — every thread on an
/// artifact, open and resolved, each with its comments.
pub(super) async fn list_branch_threads(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<ThreadDto>>> {
    let (_, a) = branch_artifact(&st, &key, &name).await?;
    let threads = discussion::list_for_artifact(&st.db, a.id, true).await?;
    Ok(Json(threads.iter().map(thread_dto).collect()))
}

/// `POST /branches/{id}/artifacts/{name}/threads` — open a new thread anchored
/// to a quoted span, seeded with its first comment. Author is `"agent"` — the
/// CLI is the only caller of the branch-scoped route.
pub(super) async fn create_branch_thread(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Json(body): Json<NewThreadBody>,
) -> ApiResult<Json<ThreadDto>> {
    let (branch, a) = branch_artifact(&st, &key, &name).await?;
    let thread = discussion::create_thread(
        &st.db,
        &discussion::NewThread {
            artifact_id: a.id,
            base_rev: body.base_rev,
            anchor_quote: &body.anchor.quote,
            anchor_prefix: &body.anchor.prefix,
            anchor_suffix: &body.anchor.suffix,
            author: "agent",
            body: &body.body,
        },
    )
    .await?;
    tracing::info!(artifact = %name, thread = thread.id, "comment posted");
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_added",
        json!({ "artifact": name, "thread": thread.id, "seq": 1, "author": "agent" }),
    )
    .await
    .ok();
    Ok(Json(thread_dto(&thread)))
}

/// `POST /branches/{id}/artifacts/{name}/threads/{tid}/comments` — append a
/// reply to an existing thread. Author is `"agent"`.
pub(super) async fn add_branch_thread_comment(
    State(st): State<AppState>,
    Path((key, name, tid)): Path<(String, String, i64)>,
    Json(body): Json<NewCommentBody>,
) -> ApiResult<Json<CommentDto>> {
    let (branch, _a, thread) = branch_thread(&st, &key, &name, tid).await?;
    let comment = discussion::add_comment(&st.db, thread.id, "agent", &body.body).await?;
    tracing::info!(artifact = %name, thread = thread.id, seq = comment.seq, "comment posted");
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_added",
        json!({ "artifact": name, "thread": thread.id, "seq": comment.seq, "author": "agent" }),
    )
    .await
    .ok();
    Ok(Json(CommentDto {
        seq: comment.seq,
        author: comment.author,
        body: comment.body,
        created_at: comment.created_at,
    }))
}

/// `POST /branches/{id}/artifacts/{name}/threads/{tid}/resolve` — mark a
/// thread resolved.
pub(super) async fn resolve_branch_thread(
    State(st): State<AppState>,
    Path((key, name, tid)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let (branch, _a, thread) = branch_thread(&st, &key, &name, tid).await?;
    discussion::resolve(&st.db, thread.id).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_resolved",
        json!({ "artifact": name, "thread": thread.id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "resolved": true })))
}
