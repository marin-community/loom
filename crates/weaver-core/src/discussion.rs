//! Discussion: resolvable comment threads anchored to a span of an artifact.
//!
//! A thread anchors to **quoted text** (a W3C-style text-quote selector —
//! `anchor_quote` plus a little `anchor_prefix`/`anchor_suffix` context for
//! disambiguation), not a character offset, so it survives edits made
//! elsewhere in the document. `base_rev` records the
//! [`crate::artifact::ArtifactVersion`] revision the anchor was taken from;
//! callers can detect when an anchor's quote no longer appears in the latest
//! revision and mark the thread `orphaned` via [`set_status`].
//!
//! Storage is two tables ([`crate::migrations`] 0011): an `artifact_threads`
//! envelope row and an append-only `artifact_comments` log, oldest comment
//! first (`seq` 1-based per thread). The feature is artifact-general — it
//! works on any artifact (plan, goal, report, ...), keyed only by
//! `artifact_id`.

use anyhow::Result;
use sqlx::FromRow;

use crate::db::{now_iso, Db};

/// A comment thread anchored to a span of an artifact, with its comments
/// assembled oldest-first.
#[derive(Debug, Clone)]
pub struct Thread {
    pub id: i64,
    pub artifact_id: i64,
    /// The artifact revision the anchor was taken from.
    pub base_rev: i64,
    pub anchor_quote: String,
    pub anchor_prefix: String,
    pub anchor_suffix: String,
    /// `"open"` | `"resolved"` | `"orphaned"`.
    pub status: String,
    pub created_at: String,
    /// Set when `status` moved to `"resolved"`; `None` otherwise.
    pub resolved_at: Option<String>,
    /// Oldest-first.
    pub comments: Vec<Comment>,
}

/// One reply in a thread, addressed by its 1-based `seq` within that thread.
#[derive(Debug, Clone, FromRow)]
pub struct Comment {
    pub seq: i64,
    /// `"agent"` | `"user"`.
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// The raw columns of an `artifact_threads` row — everything on [`Thread`]
/// except the assembled `comments`, which is fetched separately and attached
/// by the caller.
#[derive(Debug, Clone, FromRow)]
struct ThreadRow {
    id: i64,
    artifact_id: i64,
    base_rev: i64,
    anchor_quote: String,
    anchor_prefix: String,
    anchor_suffix: String,
    status: String,
    created_at: String,
    resolved_at: Option<String>,
}

impl ThreadRow {
    fn into_thread(self, comments: Vec<Comment>) -> Thread {
        Thread {
            id: self.id,
            artifact_id: self.artifact_id,
            base_rev: self.base_rev,
            anchor_quote: self.anchor_quote,
            anchor_prefix: self.anchor_prefix,
            anchor_suffix: self.anchor_suffix,
            status: self.status,
            created_at: self.created_at,
            resolved_at: self.resolved_at,
            comments,
        }
    }
}

/// The columns of a thread row, as one `SELECT`. Shared by every read so the
/// projection is written once.
const SELECT_THREAD: &str = "SELECT id, artifact_id, base_rev, anchor_quote, anchor_prefix, \
     anchor_suffix, status, created_at, resolved_at FROM artifact_threads";

/// A new thread's anchor, scope, and first comment.
#[derive(Debug, Clone)]
pub struct NewThread<'a> {
    pub artifact_id: i64,
    pub base_rev: i64,
    pub anchor_quote: &'a str,
    pub anchor_prefix: &'a str,
    pub anchor_suffix: &'a str,
    /// `"agent"` | `"user"`.
    pub author: &'a str,
    /// The first comment (becomes `seq` 1).
    pub body: &'a str,
}

/// Create a thread and its first comment (`seq` 1) in one transaction.
/// Returns the persisted thread with its (single) comment attached.
pub async fn create_thread(db: &Db, new: &NewThread<'_>) -> Result<Thread> {
    let NewThread {
        artifact_id,
        base_rev,
        anchor_quote,
        anchor_prefix,
        anchor_suffix,
        author,
        body,
    } = *new;
    let now = now_iso();
    let mut tx = crate::db::begin_immediate(db).await?;

    let (thread_id,): (i64,) = sqlx::query_as(
        "INSERT INTO artifact_threads
            (artifact_id, base_rev, anchor_quote, anchor_prefix, anchor_suffix, status, created_at)
         VALUES (?, ?, ?, ?, ?, 'open', ?) RETURNING id",
    )
    .bind(artifact_id)
    .bind(base_rev)
    .bind(anchor_quote)
    .bind(anchor_prefix)
    .bind(anchor_suffix)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO artifact_comments (thread_id, seq, author, body, created_at)
         VALUES (?, 1, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(author)
    .bind(body)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    get_thread(db, thread_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("thread vanished after create"))
}

/// Append the next comment to a thread (`MAX(seq)+1`, starting at 1 for an
/// (unexpectedly) empty thread). Returns the new comment.
pub async fn add_comment(db: &Db, thread_id: i64, author: &str, body: &str) -> Result<Comment> {
    let now = now_iso();
    let mut tx = crate::db::begin_immediate(db).await?;

    let next_seq: i64 = {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM artifact_comments WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        max.unwrap_or(0) + 1
    };

    sqlx::query(
        "INSERT INTO artifact_comments (thread_id, seq, author, body, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(next_seq)
    .bind(author)
    .bind(body)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Comment {
        seq: next_seq,
        author: author.to_string(),
        body: body.to_string(),
        created_at: now,
    })
}

/// Mark a thread resolved: `status -> 'resolved'`, `resolved_at -> now`.
pub async fn resolve(db: &Db, thread_id: i64) -> Result<()> {
    sqlx::query("UPDATE artifact_threads SET status = 'resolved', resolved_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(thread_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Reopen a thread: `status -> 'open'`, `resolved_at -> NULL`.
pub async fn reopen(db: &Db, thread_id: i64) -> Result<()> {
    sqlx::query("UPDATE artifact_threads SET status = 'open', resolved_at = NULL WHERE id = ?")
        .bind(thread_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Set a thread's status to an arbitrary value (used to mark `'orphaned'`
/// when an anchor's quote no longer appears in the artifact's latest
/// revision). Does not touch `resolved_at`.
pub async fn set_status(db: &Db, thread_id: i64, status: &str) -> Result<()> {
    sqlx::query("UPDATE artifact_threads SET status = ? WHERE id = ?")
        .bind(status)
        .bind(thread_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Fetch one thread by id, with its comments oldest-first. `None` if no such
/// thread.
pub async fn get_thread(db: &Db, thread_id: i64) -> Result<Option<Thread>> {
    let row = sqlx::query_as::<_, ThreadRow>(&format!("{SELECT_THREAD} WHERE id = ?"))
        .bind(thread_id)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let comments = comments_for_thread(db, row.id).await?;
    Ok(Some(row.into_thread(comments)))
}

/// A thread's comments, oldest-first (`seq` ascending).
async fn comments_for_thread(db: &Db, thread_id: i64) -> Result<Vec<Comment>> {
    let rows = sqlx::query_as::<_, Comment>(
        "SELECT seq, author, body, created_at FROM artifact_comments
         WHERE thread_id = ? ORDER BY seq ASC",
    )
    .bind(thread_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Threads for an artifact, each with its comments attached. Open threads
/// sort first, then resolved/orphaned; newest-first (`created_at DESC`,
/// `id DESC` to break same-timestamp ties) within each group.
/// `include_resolved = false` returns only `status = 'open'` threads.
pub async fn list_for_artifact(
    db: &Db,
    artifact_id: i64,
    include_resolved: bool,
) -> Result<Vec<Thread>> {
    let rows: Vec<ThreadRow> = if include_resolved {
        sqlx::query_as(&format!(
            "{SELECT_THREAD} WHERE artifact_id = ? \
             ORDER BY (status != 'open') ASC, created_at DESC, id DESC"
        ))
        .bind(artifact_id)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as(&format!(
            "{SELECT_THREAD} WHERE artifact_id = ? AND status = 'open' \
             ORDER BY created_at DESC, id DESC"
        ))
        .bind(artifact_id)
        .fetch_all(db)
        .await?
    };

    let mut threads = Vec::with_capacity(rows.len());
    for row in rows {
        let comments = comments_for_thread(db, row.id).await?;
        threads.push(row.into_thread(comments));
    }
    Ok(threads)
}

/// Explicit cascade delete of every thread (and its comments) belonging to an
/// artifact. Foreign keys aren't enabled on the pool, so the `ON DELETE
/// CASCADE` in the schema won't fire — clear comments before threads.
pub async fn delete_for_artifact(db: &Db, artifact_id: i64) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
    sqlx::query(
        "DELETE FROM artifact_comments WHERE thread_id IN
            (SELECT id FROM artifact_threads WHERE artifact_id = ?)",
    )
    .bind(artifact_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM artifact_threads WHERE artifact_id = ?")
        .bind(artifact_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{self, NewRevision};

    async fn db() -> Db {
        crate::db::connect_in_memory().await.unwrap()
    }

    /// Seed a `branches` row and a branch-scoped `artifacts` row, returning
    /// the artifact id, so a thread's `artifact_id` foreign key (and a
    /// realistic `base_rev`) is satisfied.
    async fn seed_artifact(db: &Db) -> i64 {
        sqlx::query(
            "INSERT OR IGNORE INTO branches (id, repo_root, branch, base_branch)
             VALUES ('b1', '/r', 'b1', 'main')",
        )
        .execute(db)
        .await
        .unwrap();
        let a = artifact::write(
            db,
            &NewRevision {
                repo_root: "/r",
                branch_id: Some("b1"),
                name: "plan",
                kind: "markdown",
                title: "Plan",
                content: "Hello world, this is the plan body.",
                author: "agent",
            },
        )
        .await
        .unwrap();
        a.id
    }

    fn thread_seed(artifact_id: i64) -> NewThread<'static> {
        NewThread {
            artifact_id,
            base_rev: 1,
            anchor_quote: "Hello world",
            anchor_prefix: "",
            anchor_suffix: ", this is",
            author: "agent",
            body: "what did you mean by this?",
        }
    }

    #[tokio::test]
    async fn create_thread_seeds_first_comment() {
        let db = db().await;
        let artifact_id = seed_artifact(&db).await;

        let t = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();
        assert_eq!(t.artifact_id, artifact_id);
        assert_eq!(t.base_rev, 1);
        assert_eq!(t.anchor_quote, "Hello world");
        assert_eq!(t.status, "open");
        assert!(t.resolved_at.is_none());
        assert_eq!(t.comments.len(), 1);
        assert_eq!(t.comments[0].seq, 1);
        assert_eq!(t.comments[0].author, "agent");
        assert_eq!(t.comments[0].body, "what did you mean by this?");
    }

    #[tokio::test]
    async fn add_comment_increments_seq_oldest_first() {
        let db = db().await;
        let artifact_id = seed_artifact(&db).await;
        let t = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();

        let c2 = add_comment(&db, t.id, "user", "context here")
            .await
            .unwrap();
        assert_eq!(c2.seq, 2);
        assert_eq!(c2.author, "user");

        let c3 = add_comment(&db, t.id, "agent", "ah, got it").await.unwrap();
        assert_eq!(c3.seq, 3);

        let got = get_thread(&db, t.id).await.unwrap().unwrap();
        assert_eq!(
            got.comments.iter().map(|c| c.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(got.comments[1].body, "context here");
        assert_eq!(got.comments[2].author, "agent");
    }

    #[tokio::test]
    async fn resolve_then_reopen_round_trips_status() {
        let db = db().await;
        let artifact_id = seed_artifact(&db).await;
        let t = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();

        resolve(&db, t.id).await.unwrap();
        let resolved = get_thread(&db, t.id).await.unwrap().unwrap();
        assert_eq!(resolved.status, "resolved");
        assert!(resolved.resolved_at.is_some());

        reopen(&db, t.id).await.unwrap();
        let reopened = get_thread(&db, t.id).await.unwrap().unwrap();
        assert_eq!(reopened.status, "open");
        assert!(reopened.resolved_at.is_none());
    }

    #[tokio::test]
    async fn set_status_marks_orphaned() {
        let db = db().await;
        let artifact_id = seed_artifact(&db).await;
        let t = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();

        set_status(&db, t.id, "orphaned").await.unwrap();
        let got = get_thread(&db, t.id).await.unwrap().unwrap();
        assert_eq!(got.status, "orphaned");
        // set_status doesn't touch resolved_at.
        assert!(got.resolved_at.is_none());
    }

    #[tokio::test]
    async fn get_thread_returns_none_for_unknown_id() {
        let db = db().await;
        assert!(get_thread(&db, 9999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_for_artifact_filters_and_orders() {
        let db = db().await;
        let artifact_id = seed_artifact(&db).await;

        // Three threads, created in order: t1, t2 (later resolved), t3.
        let t1 = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();
        let t2 = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();
        let t3 = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();
        resolve(&db, t2.id).await.unwrap();

        // Default (open only): t3 then t1, newest-first, t2 excluded.
        let open_only = list_for_artifact(&db, artifact_id, false).await.unwrap();
        assert_eq!(
            open_only.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![t3.id, t1.id]
        );
        assert!(open_only.iter().all(|t| t.comments.len() == 1));

        // include_resolved=true: open threads first (newest-first), then
        // resolved/orphaned.
        let all = list_for_artifact(&db, artifact_id, true).await.unwrap();
        assert_eq!(
            all.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![t3.id, t1.id, t2.id]
        );
        assert_eq!(all[2].status, "resolved");

        // A different artifact contributes nothing.
        let other_artifact_id = artifact::write(
            &db,
            &NewRevision {
                repo_root: "/r",
                branch_id: Some("b1"),
                name: "report",
                kind: "markdown",
                title: "Report",
                content: "x",
                author: "agent",
            },
        )
        .await
        .unwrap()
        .id;
        assert!(list_for_artifact(&db, other_artifact_id, true)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn delete_for_artifact_removes_threads_and_comments() {
        let db = db().await;
        let artifact_id = seed_artifact(&db).await;
        let t1 = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();
        add_comment(&db, t1.id, "user", "reply").await.unwrap();
        let t2 = create_thread(&db, &thread_seed(artifact_id)).await.unwrap();

        // A thread on a different artifact must survive the delete.
        let other_artifact_id = artifact::write(
            &db,
            &NewRevision {
                repo_root: "/r",
                branch_id: Some("b1"),
                name: "report",
                kind: "markdown",
                title: "Report",
                content: "x",
                author: "agent",
            },
        )
        .await
        .unwrap()
        .id;
        let untouched = create_thread(&db, &thread_seed(other_artifact_id))
            .await
            .unwrap();

        delete_for_artifact(&db, artifact_id).await.unwrap();

        assert!(get_thread(&db, t1.id).await.unwrap().is_none());
        assert!(get_thread(&db, t2.id).await.unwrap().is_none());
        let remaining_comments: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM artifact_comments WHERE thread_id IN (?, ?)")
                .bind(t1.id)
                .bind(t2.id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(remaining_comments, 0);

        // The other artifact's thread is untouched.
        assert!(get_thread(&db, untouched.id).await.unwrap().is_some());
    }
}
