use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{
    AddReviewCommentReq, ArtifactTextAnchorDto, CreateReviewReq, ExpectedReviewRevisionReq,
    ResolveReviewCommentReq, ReviewCommentDto, ReviewDto, ReviewSubjectDto, SubmitReviewReq,
    UpdateReviewCommentReq, UpdateReviewReq,
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
    let anchor = comment.anchor();
    ReviewCommentDto {
        id: comment.id,
        subject_version: comment.subject_version.clone(),
        anchor_kind: comment.anchor_kind.clone(),
        anchor: ArtifactTextAnchorDto {
            quote: anchor.quote,
            prefix: anchor.prefix,
            suffix: anchor.suffix,
            block_index: anchor.block_index,
        },
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
            id: review.subject_id.clone(),
            key: review.subject_key.clone(),
            label: review.subject_label.clone(),
            version: review.subject_version.clone(),
            current_version: current_version.to_string(),
        },
        status: review.status.clone(),
        summary: review.summary.clone(),
        draft_revision: review.draft_revision,
        message: review::structured_message(review),
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

async fn durable_review_dto(st: &AppState, item: &review::Review) -> ApiResult<ReviewDto> {
    let artifact_id = item
        .subject_id
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("invalid artifact review subject"))?;
    let current_version = artifact::get_by_id(&st.db, artifact_id)
        .await?
        .filter(|artifact| artifact.repo_root == item.repo_root)
        .map(|artifact| artifact.rev.to_string())
        .unwrap_or_else(|| item.subject_version.clone());
    Ok(review_dto(item, &current_version))
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
            anchor: ArtifactTextAnchorDto {
                quote: thread.anchor_quote.clone(),
                prefix: thread.anchor_prefix.clone(),
                suffix: thread.anchor_suffix.clone(),
                block_index: None,
            },
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
            id: artifact.id.to_string(),
            key: artifact.name.clone(),
            label: artifact.name.clone(),
            version: thread.base_rev.to_string(),
            current_version: artifact.rev.to_string(),
        },
        status: "submitted".to_string(),
        summary: String::new(),
        draft_revision: 0,
        message: String::new(),
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
) -> ApiResult<(Artifact, String)> {
    let artifact = artifact::get(&st.db, &branch.repo_root, &branch.id, name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    let version = require_artifact_version(st, &artifact, version, "artifact review").await?;
    Ok((artifact, version))
}

async fn require_artifact_version(
    st: &AppState,
    artifact: &Artifact,
    version: &str,
    label: &str,
) -> ApiResult<String> {
    let rev = version
        .parse::<i64>()
        .map_err(|_| AppError::bad_request(format!("{label} version must be a revision number")))?;
    if artifact::version(&st.db, artifact.id, rev).await?.is_none() {
        return Err(AppError::not_found("artifact revision"));
    }
    Ok(rev.to_string())
}

async fn review_artifact(st: &AppState, review: &review::Review) -> ApiResult<Artifact> {
    if review.subject_kind != "artifact" {
        return Err(AppError::bad_request("unsupported review subject kind"));
    }
    let artifact_id = review
        .subject_id
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
    let (artifact, subject_version) =
        artifact_subject(st, branch, &body.subject_key, &body.subject_version).await?;
    let review = review::get_or_create(
        &st.db,
        &review::NewReview {
            repo_root: &branch.repo_root,
            branch_id: &branch.id,
            session_id: &session.id,
            subject_kind: "artifact",
            subject_id: &artifact.id.to_string(),
            subject_key: &artifact.name,
            subject_label: &artifact.name,
            subject_version: &subject_version,
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

async fn submitted_operator_review(
    st: &AppState,
    principal: &Principal,
    review_id: i64,
) -> ApiResult<review::Review> {
    require_operator(principal)?;
    review::get(&st.db, review_id)
        .await?
        .filter(|review| review.status == "submitted")
        .ok_or_else(|| AppError::not_found("submitted review"))
}

pub(super) async fn get_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
) -> ApiResult<Json<ReviewDto>> {
    require_operator(&principal)?;
    let item = review::get_visible(&st.db, review_id, &principal.username)
        .await?
        .ok_or_else(|| AppError::not_found("review"))?;
    Ok(Json(durable_review_dto(&st, &item).await?))
}

fn text_anchor(anchor: &ArtifactTextAnchorDto) -> review::ArtifactTextAnchor {
    review::ArtifactTextAnchor {
        quote: anchor.quote.clone(),
        prefix: anchor.prefix.clone(),
        suffix: anchor.suffix.clone(),
        block_index: anchor.block_index,
    }
}

fn require_text_anchor_kind(kind: &str) -> ApiResult<()> {
    if kind == "text" {
        Ok(())
    } else {
        Err(AppError::bad_request(
            "artifact review anchor_kind must be 'text'",
        ))
    }
}

async fn draft_mutation_error(
    st: &AppState,
    item: &review::Review,
    error: anyhow::Error,
) -> AppError {
    let drift = error
        .downcast_ref::<review::DraftRevisionConflict>()
        .is_some()
        || error.to_string().contains("draft review not found")
        || error
            .to_string()
            .contains("draft changed while applying the mutation");
    if drift {
        let fresh = match review::get(&st.db, item.id).await {
            Ok(Some(fresh)) => match review_artifact(st, &fresh).await {
                Ok(artifact) => serde_json::to_value(review_dto(&fresh, &artifact.rev.to_string()))
                    .unwrap_or(Value::Null),
                Err(_) => Value::Null,
            },
            _ => Value::Null,
        };
        return AppError::conflict(error.to_string()).with_details(json!({ "review": fresh }));
    }
    AppError::bad_request(error.to_string())
}

async fn submitted_draft_conflict(st: &AppState, item: &review::Review) -> AppError {
    let artifact = review_artifact(st, item).await.ok();
    let fresh = artifact
        .map(|artifact| {
            serde_json::to_value(review_dto(item, &artifact.rev.to_string())).unwrap_or(Value::Null)
        })
        .unwrap_or(Value::Null);
    AppError::conflict("submitted reviews are immutable").with_details(json!({ "review": fresh }))
}

pub(super) async fn add_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<AddReviewCommentReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(submitted_draft_conflict(&st, &item).await);
    }
    let artifact = review_artifact(&st, &item).await?;
    let subject_version =
        require_artifact_version(&st, &artifact, &body.subject_version, "comment").await?;
    require_text_anchor_kind(&body.anchor_kind)?;
    let updated = match review::add_comment(
        &st.db,
        item.id,
        &principal.username,
        body.expected_revision,
        &review::NewComment {
            subject_version: &subject_version,
            anchor: &text_anchor(&body.anchor),
            body: &body.body,
        },
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) => return Err(draft_mutation_error(&st, &item, error).await),
    };
    Ok(Json(review_dto(&updated, &artifact.rev.to_string())))
}

pub(super) async fn update_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((review_id, comment_id)): Path<(i64, i64)>,
    Json(body): Json<UpdateReviewCommentReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(submitted_draft_conflict(&st, &item).await);
    }
    let subject_version = if let Some(version) = &body.subject_version {
        let artifact = review_artifact(&st, &item).await?;
        Some(require_artifact_version(&st, &artifact, version, "comment").await?)
    } else {
        None
    };
    if let Some(kind) = &body.anchor_kind {
        require_text_anchor_kind(kind)?;
    }
    if body.body.is_none() && body.subject_version.is_none() && body.anchor.is_none() {
        return Err(AppError::bad_request("comment update is empty"));
    }
    if body.subject_version.is_some() && body.anchor.is_none() {
        return Err(AppError::bad_request(
            "a replacement anchor is required when changing comment revision",
        ));
    }
    if body.anchor.is_some() && body.anchor_kind.as_deref() != Some("text") {
        return Err(AppError::bad_request(
            "anchor_kind 'text' is required when replacing an anchor",
        ));
    }
    let anchor = body.anchor.as_ref().map(text_anchor);
    let updated = match review::patch_comment(
        &st.db,
        item.id,
        comment_id,
        &principal.username,
        body.expected_revision,
        &review::CommentPatch {
            subject_version: subject_version.as_deref(),
            anchor: anchor.as_ref(),
            body: body.body.as_deref(),
        },
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) => return Err(draft_mutation_error(&st, &item, error).await),
    };
    let artifact = review_artifact(&st, &updated).await?;
    Ok(Json(review_dto(&updated, &artifact.rev.to_string())))
}

pub(super) async fn delete_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((review_id, comment_id)): Path<(i64, i64)>,
    Json(body): Json<ExpectedReviewRevisionReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(submitted_draft_conflict(&st, &item).await);
    }
    let updated = match review::delete_comment(
        &st.db,
        item.id,
        comment_id,
        &principal.username,
        body.expected_revision,
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) => return Err(draft_mutation_error(&st, &item, error).await),
    };
    let artifact = review_artifact(&st, &updated).await?;
    Ok(Json(review_dto(&updated, &artifact.rev.to_string())))
}

pub(super) async fn resolve_review_comment(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((review_id, comment_id)): Path<(i64, i64)>,
    Json(body): Json<ResolveReviewCommentReq>,
) -> ApiResult<Json<ReviewCommentDto>> {
    let item = submitted_operator_review(&st, &principal, review_id).await?;
    let comment = review::set_comment_resolved(&st.db, item.id, comment_id, body.resolved)
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

pub(super) async fn update_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<UpdateReviewReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(submitted_draft_conflict(&st, &item).await);
    }
    let artifact = review_artifact(&st, &item).await?;
    let subject_version = match &body.subject_version {
        Some(version) => Some(require_artifact_version(&st, &artifact, version, "review").await?),
        None => None,
    };
    let updated = match review::update_draft(
        &st.db,
        item.id,
        &principal.username,
        body.expected_revision,
        &review::DraftPatch {
            summary: body.summary.as_deref(),
            subject_version: subject_version.as_deref(),
        },
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) => return Err(draft_mutation_error(&st, &item, error).await),
    };
    Ok(Json(review_dto(&updated, &artifact.rev.to_string())))
}

pub(super) async fn discard_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<ExpectedReviewRevisionReq>,
) -> ApiResult<Json<Value>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(submitted_draft_conflict(&st, &item).await);
    }
    if let Err(error) =
        review::discard(&st.db, item.id, &principal.username, body.expected_revision).await
    {
        return Err(draft_mutation_error(&st, &item, error).await);
    }
    Ok(Json(json!({ "discarded": true })))
}

pub(super) async fn retarget_review_to_current(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<ExpectedReviewRevisionReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    if item.status != "draft" {
        return Err(submitted_draft_conflict(&st, &item).await);
    }
    let updated = match review::retarget_draft_to_current(
        &st.db,
        item.id,
        &principal.username,
        body.expected_revision,
    )
    .await
    {
        Ok(updated) => updated,
        Err(error) => return Err(draft_mutation_error(&st, &item, error).await),
    };
    let artifact = review_artifact(&st, &updated).await?;
    Ok(Json(review_dto(&updated, &artifact.rev.to_string())))
}

pub(super) async fn submit_review(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
    Json(body): Json<SubmitReviewReq>,
) -> ApiResult<Json<ReviewDto>> {
    let item = creator_review(&st, &principal, review_id).await?;
    let submission = match review::submit(
        &st.db,
        item.id,
        &principal.username,
        body.expected_revision,
        body.acknowledge_outdated,
    )
    .await
    {
        Ok(submission) => submission,
        Err(error) => {
            if error
                .to_string()
                .contains("acknowledge the reviewed revision")
            {
                let fresh = review::get(&st.db, item.id).await?.unwrap_or(item);
                let artifact = review_artifact(&st, &fresh).await?;
                return Err(AppError::conflict(error.to_string()).with_details(json!({
                    "review": review_dto(&fresh, &artifact.rev.to_string()),
                })));
            }
            return Err(draft_mutation_error(&st, &item, error).await);
        }
    };
    if let Some(event) = submission.event {
        st.bus.publish(event);
    }
    if let Err(error) = crate::review_delivery::deliver_review(&st, item.id).await {
        tracing::warn!(review = item.id, %error, "initial review delivery attempt failed");
    }
    let refreshed = review::get(&st.db, item.id)
        .await?
        .ok_or_else(|| AppError::not_found("review"))?;
    let artifact = review_artifact(&st, &refreshed).await?;
    Ok(Json(review_dto(&refreshed, &artifact.rev.to_string())))
}

pub(super) async fn retry_review_delivery(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(review_id): Path<i64>,
) -> ApiResult<Json<ReviewDto>> {
    let item = submitted_operator_review(&st, &principal, review_id).await?;
    if item.delivery_state != "failed" {
        return Err(AppError::conflict(
            "only failed review deliveries can be retried",
        ));
    }
    review::retry_delivery(&st.db, item.id)
        .await
        .map_err(|error| AppError::conflict(error.to_string()))?;
    if let Err(error) = crate::review_delivery::deliver_review(&st, item.id).await {
        tracing::warn!(review = item.id, %error, "manual review delivery retry failed");
    }
    let refreshed = review::get(&st.db, item.id)
        .await?
        .ok_or_else(|| AppError::not_found("review"))?;
    Ok(Json(durable_review_dto(&st, &refreshed).await?))
}
