//! Durable staged reviews over versioned subjects.
//!
//! Drafts are private to their creator and scoped to the session where the
//! review was started. Comments carry their own subject revision and a generic
//! JSON anchor so artifact text selectors and future diff-line selectors share
//! the same envelope. Submission freezes the review, records one structured
//! branch event, and creates one delivery outbox row in the same transaction.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::{FromRow, Row, Sqlite};

use crate::db::{now_iso, Db};
use crate::events::Event;

pub const MAX_COMMENTS: i64 = 100;
pub const MAX_COMMENT_BYTES: usize = 8 * 1024;
pub const MAX_ANCHOR_BYTES: usize = 8 * 1024;
pub const MAX_REVIEW_BYTES: i64 = 256 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, FromRow)]
pub struct Review {
    pub id: i64,
    pub repo_root: String,
    pub branch_id: String,
    pub session_id: String,
    pub subject_kind: String,
    pub subject_key: String,
    pub subject_label: String,
    pub subject_version: String,
    pub status: String,
    pub summary: String,
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
    pub fn anchor(&self) -> Value {
        serde_json::from_str(&self.anchor_json).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone)]
pub struct NewReview<'a> {
    pub repo_root: &'a str,
    pub branch_id: &'a str,
    pub session_id: &'a str,
    pub subject_kind: &'a str,
    pub subject_key: &'a str,
    pub subject_label: &'a str,
    pub subject_version: &'a str,
    pub created_by: &'a str,
}

#[derive(Debug, Clone)]
pub struct NewComment<'a> {
    pub subject_version: &'a str,
    pub anchor_kind: &'a str,
    pub anchor: &'a Value,
    pub body: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct CommentPatch<'a> {
    pub subject_version: Option<&'a str>,
    pub anchor_kind: Option<&'a str>,
    pub anchor: Option<&'a Value>,
    pub body: Option<&'a str>,
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
}

const SELECT_REVIEW: &str = "SELECT id, repo_root, branch_id, session_id, subject_kind, \
    subject_key, subject_label, subject_version, status, summary, created_by, \
    acknowledged_outdated, delivery_state, delivery_error, delivery_key, created_at, \
    updated_at, submitted_at FROM reviews";

const SELECT_COMMENT: &str = "SELECT id, review_id, subject_version, anchor_kind, anchor_json, \
    body, status, created_at, updated_at FROM review_comments";

fn validate_comment(body: &str, anchor_kind: &str, anchor: &Value) -> Result<()> {
    let body = body.trim();
    if body.is_empty() {
        bail!("comment body is required");
    }
    if body.len() > MAX_COMMENT_BYTES {
        bail!("comment body exceeds the 8 KiB limit");
    }
    if anchor_kind.trim().is_empty() || !anchor.is_object() {
        bail!("a structured comment anchor is required");
    }
    if anchor.to_string().len() > MAX_ANCHOR_BYTES {
        bail!("comment anchor exceeds the 8 KiB limit");
    }
    Ok(())
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
            (repo_root, branch_id, session_id, subject_kind, subject_key, subject_label,
             subject_version, created_by, delivery_key, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'review:' || lower(hex(randomblob(16))), ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(new.repo_root)
    .bind(new.branch_id)
    .bind(new.session_id)
    .bind(new.subject_kind)
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
) -> Result<Review> {
    sqlx::query_as::<_, Review>(&format!(
        "{SELECT_REVIEW} WHERE id = ? AND created_by = ? AND status = 'draft'"
    ))
    .bind(review_id)
    .bind(creator)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| anyhow!("draft review not found"))
}

pub async fn add_comment(
    db: &Db,
    review_id: i64,
    creator: &str,
    new: &NewComment<'_>,
) -> Result<ReviewComment> {
    validate_comment(new.body, new.anchor_kind, new.anchor)?;
    let body = new.body.trim();
    let anchor_json = new.anchor.to_string();
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator).await?;
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
    let row = sqlx::query_as::<_, ReviewComment>(
        "INSERT INTO review_comments
            (review_id, subject_version, anchor_kind, anchor_json, body, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         RETURNING id, review_id, subject_version, anchor_kind, anchor_json, body, status,
                   created_at, updated_at",
    )
    .bind(review_id)
    .bind(new.subject_version)
    .bind(new.anchor_kind)
    .bind(anchor_json)
    .bind(body)
    .bind(&now)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE reviews SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn patch_comment(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
    patch: &CommentPatch<'_>,
) -> Result<ReviewComment> {
    if let Some(body) = patch.body {
        if body.trim().is_empty() {
            bail!("comment body is required");
        }
        if body.len() > MAX_COMMENT_BYTES {
            bail!("comment body exceeds the 8 KiB limit");
        }
    }
    if patch.anchor.is_some() && patch.anchor_kind.is_none() {
        bail!("anchor_kind is required when replacing an anchor");
    }
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator).await?;
    let current = sqlx::query_as::<_, ReviewComment>(&format!(
        "{SELECT_COMMENT} WHERE id = ? AND review_id = ?"
    ))
    .bind(comment_id)
    .bind(review_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("review comment not found"))?;

    let subject_version = patch.subject_version.unwrap_or(&current.subject_version);
    let anchor_kind = patch.anchor_kind.unwrap_or(&current.anchor_kind);
    let anchor_json = patch
        .anchor
        .map(Value::to_string)
        .unwrap_or(current.anchor_json);
    let body = patch.body.unwrap_or(&current.body).trim();
    validate_comment(
        body,
        anchor_kind,
        &serde_json::from_str(&anchor_json).unwrap_or(Value::Null),
    )?;
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
    let row = sqlx::query_as::<_, ReviewComment>(
        "UPDATE review_comments
         SET subject_version = ?, anchor_kind = ?, anchor_json = ?, body = ?, updated_at = ?
         WHERE id = ? AND review_id = ?
         RETURNING id, review_id, subject_version, anchor_kind, anchor_json, body, status,
                   created_at, updated_at",
    )
    .bind(subject_version)
    .bind(anchor_kind)
    .bind(anchor_json)
    .bind(body)
    .bind(&now)
    .bind(comment_id)
    .bind(review_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE reviews SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn delete_comment(
    db: &Db,
    review_id: i64,
    comment_id: i64,
    creator: &str,
) -> Result<bool> {
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator).await?;
    let removed = sqlx::query("DELETE FROM review_comments WHERE id = ? AND review_id = ?")
        .bind(comment_id)
        .bind(review_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        == 1;
    if removed {
        sqlx::query("UPDATE reviews SET updated_at = ? WHERE id = ?")
            .bind(now_iso())
            .bind(review_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(removed)
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

pub async fn discard(db: &Db, review_id: i64, creator: &str) -> Result<bool> {
    let mut tx = crate::db::begin_immediate(db).await?;
    require_creator_draft(&mut tx, review_id, creator).await?;
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
    summary: &str,
    current_version: &str,
    acknowledge_outdated: bool,
) -> Result<Submission> {
    validate_summary(summary)?;
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
    if comments.is_empty() && summary.trim().is_empty() {
        bail!("add a comment or overall note before submitting");
    }
    let outdated = is_outdated(&review, &comments, current_version);
    if outdated && !acknowledge_outdated {
        bail!("review is outdated; acknowledge the reviewed revision before submitting");
    }

    let now = now_iso();
    sqlx::query(
        "UPDATE reviews
         SET status = 'submitted', summary = ?, acknowledged_outdated = ?,
             delivery_state = 'queued', delivery_error = NULL, submitted_at = ?, updated_at = ?
         WHERE id = ? AND status = 'draft'",
    )
    .bind(summary.trim())
    .bind(outdated && acknowledge_outdated)
    .bind(&now)
    .bind(&now)
    .bind(review_id)
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
    let event_data = json!({
        "review_id": review_id,
        "delivery_key": review.delivery_key,
        "created_by": creator,
        "session_id": review.session_id,
        "subject": {
            "kind": review.subject_kind,
            "key": review.subject_key,
            "label": review.subject_label,
            "revision": review.subject_version,
            "current_revision": current_version,
        },
        "outdated": outdated,
        "summary": summary.trim(),
        "comments": comment_payload,
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
        "SELECT review_id, delivery_key, state, attempts, next_attempt_at, last_error
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
pub async fn claim_delivery(db: &Db, review_id: i64) -> Result<bool> {
    let now = now_iso();
    let lease_until = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(now_iso);
    let mut tx = crate::db::begin_immediate(db).await?;
    let claimed = sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivering', next_attempt_at = ?
         WHERE review_id = ? AND next_attempt_at <= ?
           AND state IN ('queued', 'retrying', 'delivering')",
    )
    .bind(lease_until)
    .bind(review_id)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if claimed {
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

pub async fn mark_retry(db: &Db, review_id: i64, error: &str) -> Result<String> {
    let next = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(1))
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(now_iso);
    let mut tx = crate::db::begin_immediate(db).await?;
    let attempts = sqlx::query_scalar::<_, i64>(
        "SELECT attempts FROM review_delivery_outbox WHERE review_id = ?",
    )
    .bind(review_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("review delivery outbox item not found"))?
        + 1;
    let state = if attempts >= 3 { "failed" } else { "retrying" };
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = ?, attempts = ?, next_attempt_at = ?, last_error = ?
         WHERE review_id = ?",
    )
    .bind(state)
    .bind(attempts)
    .bind(next)
    .bind(error)
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE reviews SET delivery_state = ?, delivery_error = ? WHERE id = ?")
        .bind(state)
        .bind(error)
        .bind(review_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(state.to_string())
}

pub async fn mark_delivered(db: &Db, review_id: i64) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivered', attempts = attempts + 1, last_error = NULL
         WHERE review_id = ?",
    )
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE reviews SET delivery_state = 'delivered', delivery_error = NULL WHERE id = ?",
    )
    .bind(review_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
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
         SET state = 'queued', next_attempt_at = ?, last_error = NULL
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
        let quote = anchor
            .get("quote")
            .and_then(Value::as_str)
            .unwrap_or_default();
        message.push_str(&format!(
            "\n\n{}. Revision {}, {} anchor",
            index + 1,
            comment.subject_version,
            comment.anchor_kind
        ));
        if !quote.is_empty() {
            message.push_str(&format!(" “{}”", quote.replace('\n', " ")));
        }
        message.push_str(":\n");
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
                subject_key: &artifact.id.to_string(),
                subject_label: &artifact.name,
                subject_version: "1",
                created_by: user,
            },
        )
        .await
        .unwrap()
    }

    fn comment<'a>(body: &'a str, anchor: &'a Value) -> NewComment<'a> {
        NewComment {
            subject_version: "1",
            anchor_kind: "text",
            anchor,
            body,
        }
    }

    #[tokio::test]
    async fn drafts_are_durable_and_creator_isolated() {
        let (db, artifact) = seeded().await;
        let first = draft(&db, &artifact, "alice").await;
        add_comment(
            &db,
            first.id,
            "alice",
            &comment("change this", &json!({"quote": "beta", "block_index": 0})),
        )
        .await
        .unwrap();
        let reloaded = draft(&db, &artifact, "alice").await;
        assert_eq!(reloaded.id, first.id);
        assert_eq!(reloaded.comments.len(), 1);
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
                &artifact.id.to_string(),
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
        add_comment(
            &db,
            first_draft.id,
            "alice",
            &comment("change this", &json!({"quote": "beta", "block_index": 0})),
        )
        .await
        .unwrap();

        let stale = submit(&db, first_draft.id, "alice", "", "2", false)
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("outdated"));
        let submitted = submit(&db, first_draft.id, "alice", "Overall note", "2", true)
            .await
            .unwrap();
        assert!(submitted.event.is_some());
        assert_eq!(submitted.review.status, "submitted");
        assert!(submitted.review.acknowledged_outdated);

        let retried = submit(
            &db,
            first_draft.id,
            "alice",
            "ignored retry body",
            "2",
            true,
        )
        .await
        .unwrap();
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
        add_comment(
            &db,
            draft.id,
            "alice",
            &comment("change this", &json!({"quote": "beta", "block_index": 0})),
        )
        .await
        .unwrap();
        submit(&db, draft.id, "alice", "", "1", false)
            .await
            .unwrap();

        assert!(claim_delivery(&db, draft.id).await.unwrap());
        assert!(!claim_delivery(&db, draft.id).await.unwrap());
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(state, "delivering");
    }

    #[tokio::test]
    async fn anchors_are_bounded_individually_and_in_the_review_total() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        let oversized = json!({"quote": "x".repeat(MAX_ANCHOR_BYTES)});
        let error = add_comment(&db, draft.id, "alice", &comment("bounded body", &oversized))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("anchor exceeds"));

        let body = "b".repeat(MAX_COMMENT_BYTES);
        let near_limit = json!({"quote": "x".repeat(MAX_ANCHOR_BYTES - 16)});
        for _ in 0..16 {
            add_comment(&db, draft.id, "alice", &comment(&body, &near_limit))
                .await
                .unwrap();
        }
        let error = add_comment(&db, draft.id, "alice", &comment(&body, &near_limit))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("256 KiB total"));
    }

    #[tokio::test]
    async fn delivery_failure_state_and_manual_retry_are_honest() {
        let (db, artifact) = seeded().await;
        let draft = draft(&db, &artifact, "alice").await;
        add_comment(
            &db,
            draft.id,
            "alice",
            &comment("change this", &json!({"quote": "beta", "block_index": 0})),
        )
        .await
        .unwrap();
        submit(&db, draft.id, "alice", "", "1", false)
            .await
            .unwrap();

        let error = retry_delivery(&db, draft.id, "alice").await.unwrap_err();
        assert!(error.to_string().contains("failed review delivery"));
        assert_eq!(
            mark_retry(&db, draft.id, "first failure").await.unwrap(),
            "retrying"
        );
        assert_eq!(
            mark_retry(&db, draft.id, "second failure").await.unwrap(),
            "retrying"
        );
        assert_eq!(
            mark_retry(&db, draft.id, "third failure").await.unwrap(),
            "failed"
        );

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
}
