use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{
    AddReviewCommentReq, CreateReviewReq, ResolveReviewCommentReq, ReviewCommentDto, ReviewDto,
    ReviewSubjectDto, SubmitReviewReq, UpdateReviewCommentReq,
};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch::Branch;
use weaver_core::{discussion, review};

use crate::auth::Principal;
use crate::events;
use crate::session::Session;

use super::{require_branch, require_session, ApiResult, AppError, AppState};

#[derive(Debug, Deserialize)]
pub(super) struct ReviewListQuery {
    pub subject_kind: String,
    pub subject_key: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

fn require_operator(principal: &Principal) -> ApiResult<()> {
    if principal.is_admin() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "agent credentials cannot manage private review drafts",
        ))
    }
}

fn comment_dto(comment: &review::ReviewComment) -> ReviewCommentDto {
    ReviewCommentDto {
        id: comment.id,
        subject_version: comment.subject_version.clone(),
        anchor_kind: comment.anchor_kind.clone(),
        anchor: comment.anchor(),
        body: comment.body.clone(),
        status: comment.status.clone(),
        created_at: comment.created_at.clone(),
        updated_at: comment.updated_at.clone(),
    }
}

fn review_dto(review: &review::Review, current_version: &str) -> ReviewDto {
    let outdated = review.subject_version != current_version
        || review
            .comments
            .iter()
            .any(|comment| comment.subject_version != current_version);
    ReviewDto {
        id: review.id,
        session_id: review.session_id.clone(),
        subject: ReviewSubjectDto {
            kind: review.subject_kind.clone(),
            key: review.subject_key.clone(),
            label: review.subject_label.clone(),
            version: review.subject_version.clone(),
            current_version: current_version.to_string(),
        },
        status: review.status.clone(),
        summary: review.summary.clone(),
        created_by: review.created_by.clone(),
        outdated,
        acknowledged_outdated: review.acknowledged_outdated,
        delivery_state: review.delivery_state.clone(),
        delivery_error: review.delivery_error.clone(),
        delivery_key: review.delivery_key.clone(),
        created_at: review.created_at.clone(),
        updated_at: review.updated_at.clone(),
        submitted_at: review.submitted_at.clone(),
        comments: review.comments.iter().map(comment_dto).collect(),
        legacy: false,
    }
}

fn legacy_review_dto(
    thread: &discussion::Thread,
    session_id: &str,
    artifact: &Artifact,
) -> ReviewDto {
    let comments = thread
        .comments
        .iter()
        .map(|comment| ReviewCommentDto {
            id: -(thread.id.saturating_mul(10_000) + comment.seq),
            subject_version: thread.base_rev.to_string(),
            anchor_kind: "text".to_string(),
            anchor: json!({
                "quote": thread.anchor_quote,
                "prefix": thread.anchor_prefix,
                "suffix": thread.anchor_suffix,
                "block_index": Value::Null,
            }),
            body: comment.body.clone(),
            status: thread.status.clone(),
            created_at: comment.created_at.clone(),
            updated_at: comment.created_at.clone(),
        })
        .collect();
    ReviewDto {
        id: -thread.id,
        session_id: session_id.to_string(),
        subject: ReviewSubjectDto {
            kind: "artifact".to_string(),
            key: artifact.id.to_string(),
            label: artifact.name.clone(),
            version: thread.base_rev.to_string(),
            current_version: artifact.rev.to_string(),
        },
        status: "submitted".to_string(),
        summary: String::new(),
        created_by: thread
            .comments
            .first()
            .map(|comment| comment.author.clone())
            .unwrap_or_else(|| "legacy".to_string()),
        outdated: thread.base_rev != artifact.rev,
        acknowledged_outdated: true,
        delivery_state: "delivered".to_string(),
        delivery_error: None,
        delivery_key: format!("legacy-thread:{}", thread.id),
        created_at: thread.created_at.clone(),
        updated_at: thread
            .comments
            .last()
            .map(|comment| comment.created_at.clone())
            .unwrap_or_else(|| thread.created_at.clone()),
        submitted_at: Some(thread.created_at.clone()),
        comments,
        legacy: true,
    }
}

async fn artifact_subject(
    st: &AppState,
    branch: &Branch,
    name: &str,
    version: &str,
) -> ApiResult<Artifact> {
    let artifact = artifact::get(&st.db, &branch.repo_root, &branch.id, name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    let rev = version
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("artifact review version must be a revision number"))?;
    if artifact::version(&st.db, artifact.id, rev).await?.is_none() {
        return Err(AppError::not_found("artifact revision"));
    }
    Ok(artifact)
}

async fn review_artifact(st: &AppState, review: &review::Review) -> ApiResult<Artifact> {
    if review.subject_kind != "artifact" {
        return Err(AppError::bad_request("unsupported review subject kind"));
    }
    let artifact_id = review
        .subject_key
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("invalid artifact review subject"))?;
    artifact::get_by_id(&st.db, artifact_id)
        .await?
        .filter(|artifact| artifact.repo_root == review.repo_root)
        .ok_or_else(|| AppError::not_found("artifact"))
}

async fn list_for(
    st: &AppState,
    principal: &Principal,
    session: &Session,
    branch: &Branch,
    q: &ReviewListQuery,
) -> ApiResult<Vec<ReviewDto>> {
    if q.subject_kind != "artifact" {
        return Err(AppError::bad_request(
            "only artifact reviews are supported in this release",
        ));
    }
    let artifact = artifact::get(&st.db, &branch.repo_root, &branch.id, &q.subject_key)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    // Session-scoped agent grants may inspect submitted compatibility feedback,
    // but never inherit their operator owner's private draft merely because the
    // username is the same.
    let viewer = if principal.is_admin() {
        principal.username.as_str()
    } else {
        ""
    };
    let reviews = review::list_visible(
        &st.db,
        &branch.id,
        &session.id,
        "artifact",
        &artifact.id.to_string(),
        viewer,
    )
    .await?;
    let mut out: Vec<ReviewDto> = reviews
        .iter()
        .map(|item| review_dto(item, &artifact.rev.to_string()))
        .collect();
    for thread in discussion::list_for_artifact(&st.db, artifact.id, true).await? {
        out.push(legacy_review_dto(&thread, &session.id, &artifact));
    }
    Ok(out)
}

pub(super) async fn list_session_reviews(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
    Query(q): Query<ReviewListQuery>,
) -> ApiResult<Json<Vec<ReviewDto>>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(
        list_for(&st, &principal, &session, &branch, &q).await?,
    ))
}

pub(super) async fn list_branch_reviews(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
    Query(q): Query<ReviewListQuery>,
) -> ApiResult<Json<Vec<ReviewDto>>> {
    let branch = require_branch(&st.db, &key).await?;
    let session_id = q
        .session_id
        .as_deref()
        .ok_or_else(|| AppError::bad_request("session_id is required"))?;
    let (session, session_branch) = require_session(&st.db, session_id).await?;
    if session_branch.id != branch.id {
        return Err(AppError::bad_request(
            "review session does not belong to this branch",
        ));
    }
    Ok(Json(
        list_for(&st, &principal, &session, &branch, &q).await?,
    ))
}

async fn create_for(
    st: &AppState,
    principal: &Principal,
    session: &Session,
    branch: &Branch,
    body: &CreateReviewReq,
) -> ApiResult<ReviewDto> {
    require_operator(principal)?;
    if body.subject_kind != "artifact" {
        return Err(AppError::bad_request(
            "only artifact reviews are supported in this release",
        ));
    }
    let artifact = artifact_subject(st, branch, &body.subject_key, &body.subject_version).await?;
    let review = review::get_or_create(
        &st.db,
        &review::NewReview {
            repo_root: &branch.repo_root,
            branch_id: &branch.id,
            session_id: &session.id,
            subject_kind: "artifact",
            subject_key: &artifact.id.to_string(),
            subject_label: &artifact.name,
            subject_version: &body.subject_version,
            created_by: &principal.username,
        },
    )
    .await?;
    Ok(review_dto(&review, &artifact.rev.to_string()))
}

pub(super) async fn create_session_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
    Json(body): Json<CreateReviewReq>,
) -> ApiResult<Json<ReviewDto>> {
    let (session, branch) = require_session(&st.db, &key).await?;
    Ok(Json(
        create_for(&st, &principal, &session, &branch, &body).await?,
    ))
}

pub(super) async fn create_branch_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
    Json(body): Json<CreateReviewReq>,
) -> ApiResult<Json<ReviewDto>> {
    let branch = require_branch(&st.db, &key).await?;
    let session_id = body
        .session_id
        .as_deref()
        .ok_or_else(|| AppError::bad_request("session_id is required"))?;
    let (session, session_branch) = require_session(&st.db, session_id).await?;
    if session_branch.id != branch.id {
        return Err(AppError::bad_request(
            "review session does not belong to this branch",
        ));
    }
    Ok(Json(
        create_for(&st, &principal, &session, &branch, &body).await?,
    ))
}

async fn creator_review(
    st: &AppState,
    principal: &Principal,
    review_id: i64,
) -> ApiResult<review::Review> {
    require_operator(principal)?;
    let review = review::get_visible(&st.db, review_id, &principal.username)
        .await?
        .filter(|review| review.created_by == principal.username)
        .ok_or_else(|| AppError::not_found("review"))?;
    Ok(review)
}

fn draft_changed(st: &AppState, review: &review::Review) {
    events::emit(
        &st.bus,
        &review.branch_id,
        "review_draft_changed",
        json!({
            "review_id": review.id,
            "session_id": review.session_id,
            "subject_kind": review.subject_kind,
            "subject_key": review.subject_key,
        }),
    );
}

pub(super) async fn add_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<AddReviewCommentReq>,
) -> ApiResult<Json<ReviewCommentDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(AppError::conflict("submitted reviews are immutable"));
    }
    let artifact = review_artifact(&st, &item).await?;
    let rev = body
        .subject_version
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("comment revision must be a number"))?;
    if artifact::version(&st.db, artifact.id, rev).await?.is_none() {
        return Err(AppError::not_found("artifact revision"));
    }
    let comment = review::add_comment(
        &st.db,
        item.id,
        &principal.username,
        &review::NewComment {
            subject_version: &body.subject_version,
            anchor_kind: &body.anchor_kind,
            anchor: &body.anchor,
            body: &body.body,
        },
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    draft_changed(&st, &item);
    Ok(Json(comment_dto(&comment)))
}

pub(super) async fn update_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((review_id, comment_id)): Path<(i64, i64)>,
    Json(body): Json<UpdateReviewCommentReq>,
) -> ApiResult<Json<ReviewCommentDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(AppError::conflict("submitted reviews are immutable"));
    }
    if let Some(version) = &body.subject_version {
        let artifact = review_artifact(&st, &item).await?;
        let rev = version
            .parse::<i64>()
            .map_err(|_| AppError::bad_request("comment revision must be a number"))?;
        if artifact::version(&st.db, artifact.id, rev).await?.is_none() {
            return Err(AppError::not_found("artifact revision"));
        }
    }
    let comment = review::patch_comment(
        &st.db,
        item.id,
        comment_id,
        &principal.username,
        &review::CommentPatch {
            subject_version: body.subject_version.as_deref(),
            anchor_kind: body.anchor_kind.as_deref(),
            anchor: body.anchor.as_ref(),
            body: body.body.as_deref(),
        },
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    draft_changed(&st, &item);
    Ok(Json(comment_dto(&comment)))
}

pub(super) async fn delete_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((review_id, comment_id)): Path<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(AppError::conflict("submitted reviews are immutable"));
    }
    if !review::delete_comment(&st.db, item.id, comment_id, &principal.username).await? {
        return Err(AppError::not_found("review comment"));
    }
    draft_changed(&st, &item);
    Ok(Json(json!({ "deleted": true })))
}

pub(super) async fn resolve_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((review_id, comment_id)): Path<(i64, i64)>,
    Json(body): Json<ResolveReviewCommentReq>,
) -> ApiResult<Json<ReviewCommentDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "submitted" {
        return Err(AppError::conflict(
            "submit the review before resolving its comments",
        ));
    }
    let comment = review::set_comment_resolved(
        &st.db,
        item.id,
        comment_id,
        &principal.username,
        body.resolved,
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    events::emit(
        &st.bus,
        &item.branch_id,
        "review_comment_resolved",
        json!({
            "review_id": item.id,
            "comment_id": comment.id,
            "resolved": body.resolved,
            "session_id": item.session_id,
            "subject_kind": item.subject_kind,
            "subject_key": item.subject_key,
        }),
    );
    Ok(Json(comment_dto(&comment)))
}

pub(super) async fn discard_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(AppError::conflict("submitted reviews are immutable"));
    }
    review::discard(&st.db, item.id, &principal.username).await?;
    draft_changed(&st, &item);
    Ok(Json(json!({ "discarded": true })))
}

pub(super) async fn submit_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<SubmitReviewReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    let artifact = review_artifact(&st, &item).await?;
    let current_version = artifact.rev.to_string();
    let outdated = item.subject_version != current_version
        || item
            .comments
            .iter()
            .any(|comment| comment.subject_version != current_version);
    if item.status == "draft" && outdated && !body.acknowledge_outdated {
        return Err(AppError::conflict(
            "review is outdated; acknowledge the reviewed revision before submitting",
        )
        .with_details(json!({
            "reviewed_revision": item.subject_version,
            "current_revision": current_version,
        })));
    }
    let submission = review::submit(
        &st.db,
        item.id,
        &principal.username,
        &body.summary,
        &current_version,
        body.acknowledge_outdated,
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    if let Some(event) = submission.event {
        st.bus.publish(event);
    }
    if let Err(error) = crate::review_delivery::deliver_review(&st, item.id).await {
        tracing::warn!(review = item.id, %error, "initial review delivery attempt failed");
    }
    let refreshed = review::get(&st.db, item.id)
        .await?
        .ok_or_else(|| AppError::not_found("review"))?;
    Ok(Json(review_dto(&refreshed, &current_version)))
}

pub(super) async fn retry_review_delivery(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "submitted" {
        return Err(AppError::conflict(
            "submit the review before retrying delivery",
        ));
    }
    review::retry_delivery(&st.db, item.id, &principal.username).await?;
    if let Err(error) = crate::review_delivery::deliver_review(&st, item.id).await {
        tracing::warn!(review = item.id, %error, "manual review delivery retry failed");
    }
    let refreshed = review::get(&st.db, item.id)
        .await?
        .ok_or_else(|| AppError::not_found("review"))?;
    let artifact = review_artifact(&st, &refreshed).await?;
    Ok(Json(review_dto(&refreshed, &artifact.rev.to_string())))
}
