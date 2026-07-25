//! Durable staged reviews over versioned subjects.
//!
//! Drafts are private to their creator and scoped to the session where the
//! review was started. Artifact comments carry their own subject revision and a
//! bounded text anchor with complete relocation context. Every draft mutation
//! advances an optimistic revision. Submission checks the current artifact
//! revision, freezes the exact structured message, records one branch event,
//! and creates one delivery-outbox row in the same immediate transaction.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, Row, Sqlite};
use std::fmt;

use crate::db::{now_iso, Db};
use crate::events::Event;

pub const MAX_COMMENTS: i64 = 100;
pub const MAX_COMMENT_BYTES: usize = 8 * 1024;
pub const MAX_ANCHOR_BYTES: usize = 8 * 1024;
pub const MAX_REVIEW_BYTES: i64 = 256 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 8 * 1024;
pub const MAX_QUOTE_BYTES: usize = 4 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactTextAnchor {
    pub quote: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub block_index: Option<i64>,
}

#[derive(Debug)]
pub struct DraftRevisionConflict {
    pub expected: i64,
    pub actual: i64,
}

impl fmt::Display for DraftRevisionConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "draft changed (expected revision {}, current revision {})",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for DraftRevisionConflict {}

#[derive(Debug, Clone, FromRow)]
pub struct Review {
    pub id: i64,
    pub repo_root: String,
    pub branch_id: String,
    pub session_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_key: String,
    pub subject_label: String,
    pub subject_version: String,
    pub status: String,
    pub summary: String,
    pub draft_revision: i64,
    pub created_by: String,
    pub acknowledged_outdated: bool,
    pub delivery_state: String,
    pub delivery_error: Option<String>,
    pub delivery_key: String,
    pub created_at: String,
    pub updated_at: String,
    pub submitted_at: Option<String>,
    #[sqlx(skip)]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ReviewComment {
    pub id: i64,
    pub review_id: i64,
    pub subject_version: String,
    pub anchor_kind: String,
    pub anchor_json: String,
    pub body: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl ReviewComment {
    pub fn anchor(&self) -> ArtifactTextAnchor {
        serde_json::from_str(&self.anchor_json).unwrap_or(ArtifactTextAnchor {
            quote: String::new(),
            prefix: String::new(),
            suffix: String::new(),
            block_index: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewReview<'a> {
    pub repo_root: &'a str,
    pub branch_id: &'a str,
    pub session_id: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub subject_key: &'a str,
    pub subject_label: &'a str,
    pub subject_version: &'a str,
    pub created_by: &'a str,
}

#[derive(Debug, Clone)]
pub struct NewComment<'a> {
    pub subject_version: &'a str,
    pub anchor: &'a ArtifactTextAnchor,
    pub body: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct CommentPatch<'a> {
    pub subject_version: Option<&'a str>,
    pub anchor: Option<&'a ArtifactTextAnchor>,
    pub body: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct DraftPatch<'a> {
    pub summary: Option<&'a str>,
    pub subject_version: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Submission {
    pub review: Review,
    pub event: Option<Event>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OutboxItem {
    pub review_id: i64,
    pub delivery_key: String,
    pub state: String,
    pub attempts: i64,
    pub next_attempt_at: String,
    pub last_error: Option<String>,
    pub lease_token: Option<String>,
    pub lease_generation: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct DeliveryLease {
    pub token: String,
    pub generation: i64,
}

const SELECT_REVIEW: &str = "SELECT id, repo_root, branch_id, session_id, subject_kind, \
    subject_id, subject_key, subject_label, subject_version, status, summary, draft_revision, created_by, \
    acknowledged_outdated, delivery_state, delivery_error, delivery_key, created_at, \
    updated_at, submitted_at FROM reviews";

const SELECT_COMMENT: &str = "SELECT id, review_id, subject_version, anchor_kind, anchor_json, \
    body, status, created_at, updated_at FROM review_comments";

fn validate_anchor(anchor: &ArtifactTextAnchor) -> Result<()> {
    if anchor.quote.trim().is_empty() {
        bail!("a non-empty text quote is required");
    }
    if anchor.quote.len() > MAX_QUOTE_BYTES {
        bail!("anchor quote exceeds the 4 KiB limit");
    }
    if anchor.prefix.len() > MAX_CONTEXT_BYTES || anchor.suffix.len() > MAX_CONTEXT_BYTES {
        bail!("anchor prefix and suffix are limited to 1 KiB each");
    }
    if anchor.block_index.is_some_and(|index| index < 0) {
        bail!("anchor block_index must be non-negative");
    }
    if serde_json::to_string(anchor)?.len() > MAX_ANCHOR_BYTES {
        bail!("comment anchor exceeds the 8 KiB limit");
    }
    Ok(())
}

fn validate_comment(body: &str, anchor: &ArtifactTextAnchor) -> Result<()> {
    let body = body.trim();
    if body.is_empty() {
        bail!("comment body is required");
    }
    if body.len() > MAX_COMMENT_BYTES {
        bail!("comment body exceeds the 8 KiB limit");
    }
    validate_anchor(anchor)
}

fn validate_summary(summary: &str) -> Result<()> {
    if summary.len() > MAX_SUMMARY_BYTES {
        bail!("review summary exceeds the 8 KiB limit");
    }
    Ok(())
}

async fn comments_for<'e, E>(executor: E, review_id: i64) -> Result<Vec<ReviewComment>>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    Ok(sqlx::query_as::<_, ReviewComment>(&format!(
        "{SELECT_COMMENT} WHERE review_id = ? ORDER BY id"
    ))
    .bind(review_id)
    .fetch_all(executor)
    .await?)
}

async fn attach_comments(db: &Db, mut review: Review) -> Result<Review> {
    review.comments = comments_for(db, review.id).await?;
    Ok(review)
}

async fn attach_comments_tx(
    tx: &mut crate::db::DbTransaction<'_>,
    mut review: Review,
) -> Result<Review> {
    review.comments = comments_for(&mut **tx, review.id).await?;
    Ok(review)
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Review>> {
    let review = sqlx::query_as::<_, Review>(&format!("{SELECT_REVIEW} WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    match review {
        Some(review) => Ok(Some(attach_comments(db, review).await?)),
        None => Ok(None),
    }
}

pub async fn get_visible(db: &Db, id: i64, viewer: &str) -> Result<Option<Review>> {
    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW} WHERE id = ? AND (status != 'draft' OR created_by = ?)"
    ))
    .bind(id)
    .bind(viewer)
    .fetch_optional(db)
    .await?;
    match review {
        Some(review) => Ok(Some(attach_comments(db, review).await?)),
        None => Ok(None),
    }
}

pub async fn list_visible(
    db: &Db,
    branch_id: &str,
    session_id: &str,
    subject_kind: &str,
    subject_key: &str,
    viewer: &str,
) -> Result<Vec<Review>> {
    let rows = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW}
         WHERE branch_id = ? AND session_id = ? AND subject_kind = ? AND subject_key = ?
           AND (status != 'draft' OR created_by = ?)
         ORDER BY (status = 'draft') DESC, id"
    ))
    .bind(branch_id)
    .bind(session_id)
    .bind(subject_kind)
    .bind(subject_key)
    .bind(viewer)
    .fetch_all(db)
    .await?;
    let mut reviews = Vec::with_capacity(rows.len());
    for review in rows {
        reviews.push(attach_comments(db, review).await?);
    }
    Ok(reviews)
}

/// Return the creator's existing draft for this exact session/subject, or
/// create it. The partial unique index is the concurrency guard.
pub async fn get_or_create(db: &Db, new: &NewReview<'_>) -> Result<Review> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO reviews
            (repo_root, branch_id, session_id, subject_kind, subject_id, subject_key, subject_label,
             subject_version, created_by, delivery_key, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'review:' || lower(hex(randomblob(16))), ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(new.repo_root)
    .bind(new.branch_id)
    .bind(new.session_id)
    .bind(new.subject_kind)
    .bind(new.subject_id)
    .bind(new.subject_key)
    .bind(new.subject_label)
    .bind(new.subject_version)
    .bind(new.created_by)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;

    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW}
         WHERE created_by = ? AND session_id = ? AND subject_kind = ? AND subject_key = ?
           AND status = 'draft'"
    ))
    .bind(new.created_by)
    .bind(new.session_id)
    .bind(new.subject_kind)
    .bind(new.subject_key)
    .fetch_one(db)
    .await?;
    attach_comments(db, review).await
}

async fn require_creator_draft(
    tx: &mut crate::db::DbTransaction<'_>,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<Review> {
    let review = sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW} WHERE id = ? AND created_by = ? AND status = 'draft'"
    ))
    .bind(review_id)
    .bind(creator)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow!("draft review not found"))?;
    if review.draft_revision != expected_revision {
        return Err(anyhow!(DraftRevisionConflict {
            expected: expected_revision,
            actual: review.draft_revision,
        }));
    }
    Ok(review)
}

async fn finish_draft_mutation(
    tx: &mut crate::db::DbTransaction<'_>,
    review_id: i64,
    expected_revision: i64,
    now: &str,
) -> Result<Review> {
    let updated = sqlx::query(
        "UPDATE reviews
         SET draft_revision = draft_revision + 1, updated_at = ?
         WHERE id = ? AND status = 'draft' AND draft_revision = ?",
    )
    .bind(now)
    .bind(review_id)
    .bind(expected_revision)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        bail!("draft changed while applying the mutation");
    }
    let review = sqlx::query_as::<_, Review>(&format!("{SELECT_REVIEW} WHERE id = ?"))
        .bind(review_id)
        .fetch_one(&mut **tx)
        .await?;
    attach_comments_tx(tx, review).await
}

pub async fn add_comment(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
    new: &NewComment<'_>,
) -> Result<Review> {
    validate_comment(new.body, new.anchor)?;
    let body = new.body.trim();
    let anchor_json = serde_json::to_string(new.anchor)?;
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator, expected_revision).await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM review_comments WHERE review_id = ?")
        .bind(review_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_COMMENTS {
        bail!("a review may contain at most 100 comments");
    }
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(
             length(CAST(body AS BLOB)) + length(CAST(anchor_json AS BLOB))
         ), 0)
         FROM review_comments WHERE review_id = ?",
    )
    .bind(review_id)
    .fetch_one(&mut *tx)
    .await?;
    if total + body.len() as i64 + anchor_json.len() as i64 > MAX_REVIEW_BYTES {
        bail!("review comments and anchors exceed the 256 KiB total limit");
    }
    let now = now_iso();
    sqlx::query(
        "INSERT INTO review_comments
            (review_id, subject_version, anchor_kind, anchor_json, body, created_at, updated_at)
         VALUES (?, ?, 'text', ?, ?, ?, ?)",
    )
    .bind(review_id)
    .bind(new.subject_version)
    .bind(anchor_json)
    .bind(body)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    let review = finish_draft_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

pub async fn patch_comment(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
    expected_revision: i64,
    patch: &CommentPatch<'_>,
) -> Result<Review> {
    if patch.body.is_none() && patch.subject_version.is_none() && patch.anchor.is_none() {
        bail!("comment update is empty");
    }
    if patch.subject_version.is_some() && patch.anchor.is_none() {
        bail!("a replacement anchor is required when changing comment revision");
    }
    if let Some(body) = patch.body {
        if body.trim().is_empty() {
            bail!("comment body is required");
        }
        if body.len() > MAX_COMMENT_BYTES {
            bail!("comment body exceeds the 8 KiB limit");
        }
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator, expected_revision).await?;
    let current = sqlx::query_as::<_, ReviewComment>(&format!(
        "{SELECT_COMMENT} WHERE id = ? AND review_id = ?"
    ))
    .bind(comment_id)
    .bind(review_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("review comment not found"))?;

    let subject_version = patch.subject_version.unwrap_or(&current.subject_version);
    let anchor_json = patch
        .anchor
        .map(serde_json::to_string)
        .transpose()?
        .unwrap_or(current.anchor_json);
    let body = patch.body.unwrap_or(&current.body).trim();
    let anchor: ArtifactTextAnchor =
        serde_json::from_str(&anchor_json).context("decoding stored review anchor")?;
    validate_comment(body, &anchor)?;
    let total_without: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(
             length(CAST(body AS BLOB)) + length(CAST(anchor_json AS BLOB))
         ), 0)
         FROM review_comments WHERE review_id = ? AND id != ?",
    )
    .bind(review_id)
    .bind(comment_id)
    .fetch_one(&mut *tx)
    .await?;
    if total_without + body.len() as i64 + anchor_json.len() as i64 > MAX_REVIEW_BYTES {
        bail!("review comments and anchors exceed the 256 KiB total limit");
    }
    let now = now_iso();
    sqlx::query(
        "UPDATE review_comments
         SET subject_version = ?, anchor_kind = 'text', anchor_json = ?, body = ?, updated_at = ?
         WHERE id = ? AND review_id = ?",
    )
    .bind(subject_version)
    .bind(anchor_json)
    .bind(body)
    .bind(&now)
    .bind(comment_id)
    .bind(review_id)
    .execute(&mut *tx)
    .await?;

    // Re-anchoring the final old comment is an explicit transition of the
    // whole review envelope (including its persisted overall note) onto that
    // revision. Any remaining old anchor keeps the original target and stale
    // acknowledgement requirement.
    if patch.subject_version.is_some() && patch.anchor.is_some() {
        let old_comments: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM review_comments
             WHERE review_id = ? AND subject_version != ?",
        )
        .bind(review_id)
        .bind(subject_version)
        .fetch_one(&mut *tx)
        .await?;
        if old_comments == 0 {
            sqlx::query("UPDATE reviews SET subject_version = ? WHERE id = ?")
                .bind(subject_version)
                .bind(review_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    let review = finish_draft_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

pub async fn delete_comment(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<Review> {
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator, expected_revision).await?;
    let removed = sqlx::query("DELETE FROM review_comments WHERE id = ? AND review_id = ?")
        .bind(comment_id)
        .bind(review_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        == 1;
    if !removed {
        bail!("review comment not found");
    }
    let now = now_iso();
    let review = finish_draft_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

pub async fn update_draft(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
    patch: &DraftPatch<'_>,
) -> Result<Review> {
    if let Some(summary) = patch.summary {
        validate_summary(summary)?;
    }
    if patch.summary.is_none() && patch.subject_version.is_none() {
        bail!("draft update is empty");
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator, expected_revision).await?;
    if let Some(summary) = patch.summary {
        sqlx::query("UPDATE reviews SET summary = ? WHERE id = ?")
            .bind(summary.trim())
            .bind(review_id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(subject_version) = patch.subject_version {
        let old_comments: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM review_comments
             WHERE review_id = ? AND subject_version != ?",
        )
        .bind(review_id)
        .bind(subject_version)
        .fetch_one(&mut *tx)
        .await?;
        if old_comments != 0 {
            bail!("re-anchor every comment before advancing the review revision");
        }
        sqlx::query("UPDATE reviews SET subject_version = ? WHERE id = ?")
            .bind(subject_version)
            .bind(review_id)
            .execute(&mut *tx)
            .await?;
    }
    let now = now_iso();
    let review = finish_draft_mutation(&mut tx, review_id, expected_revision, &now).await?;
    tx.commit().await?;
    Ok(review)
}

/// Resolution is the one mutable bit of submitted thread state. Feedback
/// content and anchors remain frozen.
pub async fn set_comment_resolved(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
    resolved: bool,
) -> Result<ReviewComment> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let review_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM reviews
           WHERE id = ? AND created_by = ? AND status = 'submitted'
         )",
    )
    .bind(review_id)
    .bind(creator)
    .fetch_one(&mut *tx)
    .await?;
    if !review_exists {
        bail!("submitted review not found");
    }
    let now = now_iso();
    let row = sqlx::query_as::<_, ReviewComment>(
        "UPDATE review_comments SET status = ?, updated_at = ?
         WHERE id = ? AND review_id = ? AND status IN ('submitted', 'resolved')
         RETURNING id, review_id, subject_version, anchor_kind, anchor_json, body, status,
                   created_at, updated_at",
    )
    .bind(if resolved { "resolved" } else { "submitted" })
    .bind(&now)
    .bind(comment_id)
    .bind(review_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("submitted review comment not found"))?;
    tx.commit().await?;
    Ok(row)
}

pub async fn discard(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
) -> Result<bool> {
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator, expected_revision).await?;
    sqlx::query("DELETE FROM review_comments WHERE review_id = ?")
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    let removed = sqlx::query("DELETE FROM reviews WHERE id = ? AND status = 'draft'")
        .bind(review_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        == 1;
    tx.commit().await?;
    Ok(removed)
}

fn is_outdated(review: &Review, comments: &[ReviewComment], current_version: &str) -> bool {
    review.subject_version != current_version
        || comments
            .iter()
            .any(|comment| comment.subject_version != current_version)
}

pub async fn submit(
    db: &Db,
    review_id: i64,
    creator: &str,
    expected_revision: i64,
    acknowledge_outdated: bool,
) -> Result<Submission> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let mut review =
        sqlx::query_as::<_, Review>(&format!("{SELECT_REVIEW} WHERE id = ? AND created_by = ?"))
            .bind(review_id)
            .bind(creator)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("review not found"))?;
    let comments = comments_for(&mut *tx, review_id).await?;

    // A retried submission is a read: never insert another event or outbox row.
    if review.status == "submitted" {
        review.comments = comments;
        tx.commit().await?;
        return Ok(Submission {
            review,
            event: None,
        });
    }
    if review.status != "draft" {
        bail!("review is not editable");
    }
    if review.draft_revision != expected_revision {
        return Err(anyhow!(DraftRevisionConflict {
            expected: expected_revision,
            actual: review.draft_revision,
        }));
    }
    if comments.is_empty() && review.summary.trim().is_empty() {
        bail!("add a comment or overall note before submitting");
    }
    if review.subject_kind != "artifact" {
        bail!("unsupported review subject kind");
    }
    let artifact_id: i64 = review
        .subject_id
        .parse()
        .context("invalid artifact review subject id")?;
    // Artifact writes use the same immediate transaction discipline. Reading
    // the current revision here makes the stale decision, immutable event, and
    // outbox insertion one serializable decision with respect to a write.
    let current_version = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(rev) FROM artifact_versions WHERE artifact_id = ?",
    )
    .bind(artifact_id)
    .fetch_one(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("artifact review subject not found"))?
    .to_string();
    let outdated = is_outdated(&review, &comments, &current_version);
    if outdated && !acknowledge_outdated {
        bail!("review is outdated; acknowledge the reviewed revision before submitting");
    }

    let now = now_iso();
    sqlx::query(
        "UPDATE reviews
         SET status = 'submitted', acknowledged_outdated = ?,
             delivery_state = 'queued', delivery_error = NULL, submitted_at = ?, updated_at = ?
         WHERE id = ? AND status = 'draft' AND draft_revision = ?",
    )
    .bind(outdated && acknowledge_outdated)
    .bind(&now)
    .bind(&now)
    .bind(review_id)
    .bind(expected_revision)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE review_comments SET status = 'submitted' WHERE review_id = ?")
        .bind(review_id)
        .execute(&mut *tx)
        .await?;

    let comment_payload: Vec<Value> = comments
        .iter()
        .map(|comment| {
            json!({
                "id": comment.id,
                "revision": comment.subject_version,
                "anchor_kind": comment.anchor_kind,
                "anchor": comment.anchor(),
                "body": comment.body,
            })
        })
        .collect();
    review.comments = comments.clone();
    let message = structured_message(&review);
    let event_data = json!({
        "review_id": review_id,
        "delivery_key": review.delivery_key,
        "created_by": creator,
        "session_id": review.session_id,
        "subject": {
            "kind": review.subject_kind,
            "id": review.subject_id,
            "key": review.subject_key,
            "label": review.subject_label,
            "revision": review.subject_version,
            "current_revision": current_version,
        },
        "outdated": outdated,
        "summary": review.summary,
        "comments": comment_payload,
        "message": message,
    });
    let event_row = sqlx::query(
        "INSERT INTO events (branch_id, kind, data)
         VALUES (?, 'review_submitted', ?) RETURNING id, created_at",
    )
    .bind(&review.branch_id)
    .bind(event_data.to_string())
    .fetch_one(&mut *tx)
    .await?;
    let event = Event {
        id: event_row.get("id"),
        branch_id: review.branch_id.clone(),
        kind: "review_submitted".to_string(),
        data: event_data,
        created_at: event_row.get("created_at"),
    };
    sqlx::query(
        "INSERT INTO review_delivery_outbox
            (review_id, delivery_key, state, attempts, next_attempt_at)
         VALUES (?, ?, 'queued', 0, ?)
         ON CONFLICT(review_id) DO NOTHING",
    )
    .bind(review_id)
    .bind(&review.delivery_key)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let review = get(db, review_id)
        .await?
        .ok_or_else(|| anyhow!("submitted review vanished"))?;
    Ok(Submission {
        review,
        event: Some(event),
    })
}

pub async fn ready_outbox(db: &Db, limit: i64) -> Result<Vec<OutboxItem>> {
    Ok(sqlx::query_as::<_, OutboxItem>(
        "SELECT review_id, delivery_key, state, attempts, next_attempt_at, last_error,
                lease_token, lease_generation
         FROM review_delivery_outbox
         WHERE state IN ('queued', 'retrying', 'delivering') AND next_attempt_at <= ?
         ORDER BY next_attempt_at, review_id LIMIT ?",
    )
    .bind(now_iso())
    .bind(limit)
    .fetch_all(db)
    .await?)
}

/// Claim one due delivery for a short lease. The lease prevents the request
/// path and background sweep from sending the same terminal payload
/// concurrently, while allowing recovery if the process exits mid-delivery.
pub async fn claim_delivery(db: &Db, review_id: i64) -> Result<Option<DeliveryLease>> {
    let now = now_iso();
    let lease_until = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(now_iso);
    let mut tx = crate::db::begin_immediate(db).await?;
    let claimed = sqlx::query_as::<_, DeliveryLease>(
        "UPDATE review_delivery_outbox
         SET state = 'delivering', next_attempt_at = ?,
             lease_token = lower(hex(randomblob(16))),
             lease_generation = lease_generation + 1
         WHERE review_id = ? AND next_attempt_at <= ?
           AND state IN ('queued', 'retrying', 'delivering')
         RETURNING lease_token AS token, lease_generation AS generation",
    )
    .bind(lease_until)
    .bind(review_id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    if claimed.is_some() {
        sqlx::query(
            "UPDATE reviews
             SET delivery_state = 'delivering', delivery_error = NULL
             WHERE id = ?",
        )
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(claimed)
}

pub async fn mark_retry(
    db: &Db,
    review_id: i64,
    lease_token: &str,
    error: &str,
) -> Result<Option<String>> {
    let next = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(now_iso);
    let mut tx = crate::db::begin_immediate(db).await?;
    let attempts = sqlx::query_scalar::<_, i64>(
        "SELECT attempts FROM review_delivery_outbox
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(review_id)
    .bind(lease_token)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(attempts) = attempts.map(|attempts| attempts + 1) else {
        tx.commit().await?;
        return Ok(None);
    };
    let state = if attempts >= 3 { "failed" } else { "retrying" };
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = ?, attempts = ?, next_attempt_at = ?, last_error = ?, lease_token = NULL
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(state)
    .bind(attempts)
    .bind(next)
    .bind(error)
    .bind(review_id)
    .bind(lease_token)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE reviews SET delivery_state = ?, delivery_error = ?
         WHERE id = ? AND delivery_state = 'delivering'",
    )
    .bind(state)
    .bind(error)
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(state.to_string()))
}

pub async fn mark_delivered(db: &Db, review_id: i64, lease_token: &str) -> Result<bool> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let updated = sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivered', attempts = attempts + 1, last_error = NULL, lease_token = NULL
         WHERE review_id = ? AND state = 'delivering' AND lease_token = ?",
    )
    .bind(review_id)
    .bind(lease_token)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 1 {
        sqlx::query(
            "UPDATE reviews SET delivery_state = 'delivered', delivery_error = NULL
             WHERE id = ? AND delivery_state = 'delivering'",
        )
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(updated.rows_affected() == 1)
}

pub async fn retry_delivery(db: &Db, review_id: i64, creator: &str) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1
           FROM reviews r
           JOIN review_delivery_outbox o ON o.review_id = r.id
           WHERE r.id = ? AND r.created_by = ? AND r.status = 'submitted'
             AND r.delivery_state = 'failed' AND o.state = 'failed'
         )",
    )
    .bind(review_id)
    .bind(creator)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        bail!("failed review delivery not found");
    }
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'queued', next_attempt_at = ?, last_error = NULL, lease_token = NULL
         WHERE review_id = ?",
    )
    .bind(now_iso())
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE reviews SET delivery_state = 'queued', delivery_error = NULL WHERE id = ?")
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub fn structured_message(review: &Review) -> String {
    let mut message = format!(
        "The user submitted feedback on artifact `{}`, revision {}.",
        review.subject_label, review.subject_version
    );
    if !review.summary.trim().is_empty() {
        message.push_str("\n\nOverall:\n");
        message.push_str(review.summary.trim());
    }
    for (index, comment) in review.comments.iter().enumerate() {
        let anchor = comment.anchor();
        let anchor_json = serde_json::to_string(&anchor).unwrap_or_else(|_| "{}".to_string());
        message.push_str(&format!(
            "\n\n{}. Revision {}, {} anchor {}:\n",
            index + 1,
            comment.subject_version,
            comment.anchor_kind,
            anchor_json
        ));
        message.push_str(&comment.body);
    }
    message.push_str(&format!(
        "\n\n[review_id: {}; delivery_key: {}]",
        review.id, review.delivery_key
    ));
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{self, NewRevision};
    use crate::branch;
    use std::sync::Arc;
    use tokio::sync::Barrier;

    async fn seeded() -> (Db, crate::artifact::Artifact) {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch::upsert(&db, "/repo", "weaver/review", "main")
            .await
            .unwrap();
        let artifact = artifact::write(
            &db,
            &NewRevision {
                repo_root: "/repo",
                branch_id: Some(&branch.id),
                name: "design",
                kind: "markdown",
                title: "Design",
                content: "Alpha beta gamma",
                author: "agent",
            },
        )
        .await
        .unwrap();
        (db, artifact)
    }

    async fn draft(db: &Db, artifact: &crate::artifact::Artifact, user: &str) -> Review {
        get_or_create(
            db,
            &NewReview {
                repo_root: &artifact.repo_root,
                branch_id: artifact.branch_id.as_deref().unwrap(),
                session_id: "session-1",
                subject_kind: "artifact",
                subject_id: &artifact.id.to_string(),
                subject_key: &artifact.name,
                subject_label: &artifact.name,
                subject_version: "1",
                created_by: user,
            },
        )
        .await
        .unwrap()
    }

    fn anchor(quote: &str) -> ArtifactTextAnchor {
        ArtifactTextAnchor {
            quote: quote.to_string(),
            prefix: "before".to_string(),
            suffix: "after".to_string(),
            block_index: Some(0),
        }
    }

    async fn add(db: &Db, draft: &Review, body: &str) -> Review {
        let anchor = anchor("beta");
        add_comment(
            db,
            draft.id,
            "alice",
            draft.draft_revision,
            &NewComment {
                subject_version: "1",
                anchor: &anchor,
                body,
            },
        )
        .await
        .unwrap()
    }

    fn comment<'a>(body: &'a str, anchor: &'a ArtifactTextAnchor) -> NewComment<'a> {
        NewComment {
            subject_version: "1",
            anchor,
            body,
        }
    }

    #[tokio::test]
    async fn drafts_are_durable_and_creator_isolated() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        let updated = add(&db, &first, "change this").await;
        let updated = update_draft(
            &db,
            updated.id,
            "alice",
            updated.draft_revision,
            &DraftPatch {
                summary: Some("Durable overall note"),
                subject_version: None,
            },
        )
        .await
        .unwrap();
        let reloaded = draft(&db, &artifact, "alice").await;
        assert_eq!(reloaded.id, first.id);
        assert_eq!(reloaded.comments.len(), 1);
        assert_eq!(reloaded.summary, "Durable overall note");
        assert_eq!(reloaded.draft_revision, updated.draft_revision);
        assert!(
            get_visible(&db, first.id, "bob").await.unwrap().is_none(),
            "another operator cannot read a draft"
        );
        assert_eq!(
            list_visible(
                &db,
                artifact.branch_id.as_deref().unwrap(),
                "session-1",
                "artifact",
                &artifact.name,
                "bob",
            )
            .await
            .unwrap()
            .len(),
            0
        );
    }

    #[tokio::test]
    async fn submit_is_atomic_idempotent_and_requires_stale_acknowledgement() {
        let (db, artifact) = seeded().await;
        let first_draft = draft(&db, &artifact, "alice").await;
        let first_draft = add(&db, &first_draft, "change this").await;
        artifact::write(
            &db,
            &NewRevision {
                repo_root: "/repo",
                branch_id: artifact.branch_id.as_deref(),
                name: "design",
                kind: "markdown",
                title: "Design",
                content: "Alpha beta gamma delta",
                author: "agent",
            },
        )
        .await
        .unwrap();

        let stale = submit(
            &db,
            first_draft.id,
            "alice",
            first_draft.draft_revision,
            false,
        )
        .await
        .unwrap_err();
        assert!(stale.to_string().contains("outdated"));
        let first_draft = update_draft(
            &db,
            first_draft.id,
            "alice",
            first_draft.draft_revision,
            &DraftPatch {
                summary: Some("Overall note"),
                subject_version: None,
            },
        )
        .await
        .unwrap();
        let submitted = submit(
            &db,
            first_draft.id,
            "alice",
            first_draft.draft_revision,
            true,
        )
        .await
        .unwrap();
        assert!(submitted.event.is_some());
        assert_eq!(submitted.review.status, "submitted");
        assert!(submitted.review.acknowledged_outdated);

        let retried = submit(&db, first_draft.id, "alice", 0, true).await.unwrap();
        assert!(retried.event.is_none());
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM events WHERE branch_id = ? AND kind = 'review_submitted'",
        )
        .bind(artifact.branch_id.as_deref().unwrap())
        .fetch_one(&db)
        .await
        .unwrap();
        let outbox: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
                .bind(first_draft.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(events, 1);
        assert_eq!(outbox, 1);

        let resolved = set_comment_resolved(
            &db,
            first_draft.id,
            submitted.review.comments[0].id,
            "alice",
            true,
        )
        .await
        .unwrap();
        assert_eq!(resolved.status, "resolved");
        let reopened = set_comment_resolved(&db, first_draft.id, resolved.id, "alice", false)
            .await
            .unwrap();
        assert_eq!(reopened.status, "submitted");

        let next = draft(&db, &artifact, "alice").await;
        assert_ne!(next.id, first_draft.id);
        assert_ne!(next.delivery_key, first_draft.delivery_key);
    }

    #[tokio::test]
    async fn delivery_claim_has_a_recoverable_single_owner_lease() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let draft = add(&db, &draft, "change this").await;
        submit(&db, draft.id, "alice", draft.draft_revision, false)
            .await
            .unwrap();

        let first = claim_delivery(&db, draft.id).await.unwrap().unwrap();
        assert!(claim_delivery(&db, draft.id).await.unwrap().is_none());
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "delivering");

        sqlx::query(
            "UPDATE review_delivery_outbox
             SET next_attempt_at = '1970-01-01T00:00:00.000Z'
             WHERE review_id = ?",
        )
        .bind(draft.id)
        .execute(&db)
        .await
        .unwrap();
        let second = claim_delivery(&db, draft.id).await.unwrap().unwrap();
        assert!(second.generation > first.generation);
        assert_eq!(
            mark_retry(&db, draft.id, &first.token, "stale worker")
                .await
                .unwrap(),
            None
        );
        assert!(mark_delivered(&db, draft.id, &second.token).await.unwrap());
        assert!(!mark_delivered(&db, draft.id, &first.token).await.unwrap());
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "delivered");
    }

    #[tokio::test]
    async fn anchors_are_bounded_individually_and_in_the_review_total() {
        let (db, artifact) = seeded().await;
        let mut draft = draft(&db, &artifact, "alice").await;
        let oversized = ArtifactTextAnchor {
            quote: "x".repeat(MAX_QUOTE_BYTES + 1),
            prefix: String::new(),
            suffix: String::new(),
            block_index: None,
        };
        let error = add_comment(
            &db,
            draft.id,
            "alice",
            draft.draft_revision,
            &comment("bounded body", &oversized),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("quote exceeds"));

        let body = "b".repeat(MAX_COMMENT_BYTES);
        let near_limit = ArtifactTextAnchor {
            quote: "x".repeat(MAX_QUOTE_BYTES),
            prefix: "p".repeat(MAX_CONTEXT_BYTES),
            suffix: "s".repeat(MAX_CONTEXT_BYTES),
            block_index: Some(0),
        };
        let mut inserted = 0;
        loop {
            match add_comment(
                &db,
                draft.id,
                "alice",
                draft.draft_revision,
                &comment(&body, &near_limit),
            )
            .await
            {
                Ok(updated) => {
                    draft = updated;
                    inserted += 1;
                }
                Err(error) => {
                    assert!(error.to_string().contains("256 KiB total"));
                    break;
                }
            }
        }
        assert!(inserted > 10);
    }

    #[tokio::test]
    async fn delivery_failure_state_and_manual_retry_are_honest() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let draft = add(&db, &draft, "change this").await;
        submit(&db, draft.id, "alice", draft.draft_revision, false)
            .await
            .unwrap();

        let error = retry_delivery(&db, draft.id, "alice").await.unwrap_err();
        assert!(error.to_string().contains("failed review delivery"));
        for (index, expected) in ["retrying", "retrying", "failed"].into_iter().enumerate() {
            sqlx::query(
                "UPDATE review_delivery_outbox
                 SET next_attempt_at = '1970-01-01T00:00:00.000Z'
                 WHERE review_id = ?",
            )
            .bind(draft.id)
            .execute(&db)
            .await
            .unwrap();
            let lease = claim_delivery(&db, draft.id).await.unwrap().unwrap();
            assert_eq!(
                mark_retry(&db, draft.id, &lease.token, &format!("failure {index}"))
                    .await
                    .unwrap()
                    .as_deref(),
                Some(expected)
            );
        }

        retry_delivery(&db, draft.id, "alice").await.unwrap();
        let error = retry_delivery(&db, draft.id, "alice").await.unwrap_err();
        assert!(error.to_string().contains("failed review delivery"));
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "queued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_artifact_write_and_submit_make_one_serialized_stale_decision() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let barrier = Arc::new(Barrier::new(3));

        let submit_db = db.clone();
        let submit_barrier = barrier.clone();
        let submit_task = tokio::spawn(async move {
            submit_barrier.wait().await;
            submit(&submit_db, draft.id, "alice", draft.draft_revision, false).await
        });
        let write_db = db.clone();
        let write_barrier = barrier.clone();
        let branch_id = artifact.branch_id.clone();
        let write_task = tokio::spawn(async move {
            write_barrier.wait().await;
            artifact::write(
                &write_db,
                &NewRevision {
                    repo_root: "/repo",
                    branch_id: branch_id.as_deref(),
                    name: "design",
                    kind: "markdown",
                    title: "Design",
                    content: "concurrent revision",
                    author: "agent",
                },
            )
            .await
        });
        barrier.wait().await;
        let submission = submit_task.await.unwrap();
        write_task.await.unwrap().unwrap();

        match submission {
            Ok(submission) => {
                let current = submission.event.unwrap().data["subject"]["current_revision"]
                    .as_str()
                    .unwrap()
                    .to_string();
                assert_eq!(current, "1", "submit committed before the artifact write");
            }
            Err(error) => assert!(error.to_string().contains("outdated")),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_draft_mutation_cannot_hide_content_from_submit_preview() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "first").await;
        let barrier = Arc::new(Barrier::new(3));

        let submit_db = db.clone();
        let submit_barrier = barrier.clone();
        let submit_id = draft.id;
        let expected = draft.draft_revision;
        let submit_task = tokio::spawn(async move {
            submit_barrier.wait().await;
            submit(&submit_db, submit_id, "alice", expected, false).await
        });
        let mutate_db = db.clone();
        let mutate_barrier = barrier.clone();
        let mutation_task = tokio::spawn(async move {
            let anchor = anchor("gamma");
            mutate_barrier.wait().await;
            add_comment(
                &mutate_db,
                submit_id,
                "alice",
                expected,
                &NewComment {
                    subject_version: "1",
                    anchor: &anchor,
                    body: "concurrent",
                },
            )
            .await
        });
        barrier.wait().await;
        let submission = submit_task.await.unwrap();
        let mutation = mutation_task.await.unwrap();

        match (submission, mutation) {
            (Ok(submitted), Err(error)) => {
                assert_eq!(submitted.review.comments.len(), 1);
                assert!(error.to_string().contains("draft review not found"));
            }
            (Err(error), Ok(mutated)) => {
                assert!(error.downcast_ref::<DraftRevisionConflict>().is_some());
                assert_eq!(mutated.comments.len(), 2);
            }
            outcome => panic!("exactly one concurrent operation must win: {outcome:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_submit_retries_freeze_one_event_and_outbox_item() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "submit once").await;
        let barrier = Arc::new(Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let task_db = db.clone();
            let task_barrier = barrier.clone();
            let review_id = draft.id;
            let expected = draft.draft_revision;
            tasks.push(tokio::spawn(async move {
                task_barrier.wait().await;
                submit(&task_db, review_id, "alice", expected, false).await
            }));
        }
        barrier.wait().await;
        let first = tasks.remove(0).await.unwrap().unwrap();
        let second = tasks.remove(0).await.unwrap().unwrap();
        assert_eq!(
            usize::from(first.event.is_some()) + usize::from(second.event.is_some()),
            1
        );
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'review_submitted'")
                .fetch_one(&db)
                .await
                .unwrap();
        let outbox: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
                .bind(draft.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!((events, outbox), (1, 1));
    }

    #[tokio::test]
    async fn reanchoring_every_comment_advances_the_truthful_envelope_revision() {
        let (db, artifact) = seeded().await;
        let draft = add(&db, &draft(&db, &artifact, "alice").await, "change this").await;
        let moved_anchor = anchor("gamma");
        let moved = patch_comment(
            &db,
            draft.id,
            draft.comments[0].id,
            "alice",
            draft.draft_revision,
            &CommentPatch {
                subject_version: Some("2"),
                anchor: Some(&moved_anchor),
                body: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(moved.subject_version, "2");
        assert_eq!(moved.comments[0].subject_version, "2");
        assert!(structured_message(&moved).contains("revision 2"));
    }
}
