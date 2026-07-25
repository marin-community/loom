//! Repo-scoped issue tracker.
//!
//! An issue belongs to a **repo** (`repo_root`). Two nullable branch
//! annotations describe its relationship to the worktrees in that repo:
//!
//! * `source_branch` — the branch it was created from (provenance).
//! * `claimed_branch` — the branch currently working it. `NULL` is the
//!   *unclaimed backlog* (the fan-out pool); a branch claims an issue by
//!   stamping its name here.
//!
//! "The branch's working set" is therefore `claimed_branch = <branch>`, and the
//! per-session badge counts the same. See `docs/repo-scoped-issues.md`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::fmt;

use crate::db::{now_iso, Db};
use crate::tags::Tag;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Issue {
    pub id: i64,
    pub repo_root: String,
    pub github_repo: Option<String>,
    pub source_branch: Option<String>,
    pub claimed_branch: Option<String>,
    pub title: String,
    pub body: String,
    pub status: String,
    pub github_issue: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// Fields for a new issue. `repo_root` and `title` are required; the branch
/// annotations are optional — a repo-level backlog item leaves `claimed_branch`
/// unset.
#[derive(Debug, Clone, Default)]
pub struct NewIssue {
    pub repo_root: String,
    pub github_repo: Option<String>,
    pub source_branch: Option<String>,
    pub claimed_branch: Option<String>,
    pub title: String,
    pub body: String,
    pub github_issue: Option<i64>,
}

/// One tag to persist with a newly-created issue.
#[derive(Debug, Clone)]
pub struct NewIssueTag {
    pub key: String,
    pub value: String,
    pub note: String,
    pub set_by: String,
}

/// Create a new issue. Returns the persisted row.
pub async fn add(db: &Db, new: &NewIssue) -> Result<Issue> {
    add_with_tags(db, new, &[]).await
}

/// Create an issue and its initial tags in one transaction.
pub async fn add_with_tags(db: &Db, new: &NewIssue, tags: &[NewIssueTag]) -> Result<Issue> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let now = now_iso();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO issues
            (repo_root, github_repo, source_branch, claimed_branch,
             title, body, status, github_issue, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 'open', ?, ?, ?) RETURNING id",
    )
    .bind(&new.repo_root)
    .bind(&new.github_repo)
    .bind(&new.source_branch)
    .bind(&new.claimed_branch)
    .bind(&new.title)
    .bind(&new.body)
    .bind(new.github_issue)
    .bind(&now)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;
    for tag in tags {
        sqlx::query(
            "INSERT INTO issue_tags (issue_id, key, value, note, set_by, set_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(&tag.key)
        .bind(&tag.value)
        .bind(&tag.note)
        .bind(&tag.set_by)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    let issue = sqlx::query_as::<_, Issue>("SELECT * FROM issues WHERE id = ?")
        .bind(row.0)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(issue = issue.id, title = %issue.title, "issue created");
    Ok(issue)
}

/// One bulk mutation supported by the issue command endpoint.
#[derive(Debug, Clone)]
pub enum BulkIssueAction {
    Close,
    Reopen,
    Tag {
        key: String,
        value: String,
        note: String,
        set_by: String,
    },
    Untag {
        key: String,
    },
    Delete,
}

/// One ID or precondition that makes a bulk issue action invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueActionProblem {
    pub id: i64,
    pub code: String,
    pub error: String,
}

/// Validation failure from [`apply_bulk_action`]. No mutation has occurred.
#[derive(Debug)]
pub struct IssueActionValidationError {
    pub problems: Vec<IssueActionProblem>,
}

impl fmt::Display for IssueActionValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "issue action validation failed for {} item{}",
            self.problems.len(),
            if self.problems.len() == 1 { "" } else { "s" }
        )
    }
}

impl std::error::Error for IssueActionValidationError {}

/// Aggregate result from an atomic issue action. `issues` contains the updated
/// rows for non-delete actions; deletes return their IDs in `deleted_ids`.
#[derive(Debug)]
pub struct BulkIssueActionResult {
    pub issues: Vec<Issue>,
    pub deleted_ids: Vec<i64>,
}

/// Validate every ID and action precondition, then apply the whole command in
/// one transaction. Any validation or database failure leaves every issue
/// unchanged.
pub async fn apply_bulk_action(
    db: &Db,
    ids: &[i64],
    action: &BulkIssueAction,
) -> Result<BulkIssueActionResult> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let mut issues = Vec::with_capacity(ids.len());
    let mut problems = Vec::new();

    for id in ids {
        let issue = sqlx::query_as::<_, Issue>("SELECT * FROM issues WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(issue) = issue else {
            problems.push(IssueActionProblem {
                id: *id,
                code: "not_found".to_string(),
                error: "issue not found".to_string(),
            });
            continue;
        };
        let state_problem = match action {
            BulkIssueAction::Close if issue.status != "open" => {
                Some(("invalid_state", "issue is already closed"))
            }
            BulkIssueAction::Reopen if issue.status != "closed" => {
                Some(("invalid_state", "issue is already open"))
            }
            BulkIssueAction::Untag { key } => {
                let exists: Option<(i64,)> =
                    sqlx::query_as("SELECT 1 FROM issue_tags WHERE issue_id = ? AND key = ?")
                        .bind(issue.id)
                        .bind(key)
                        .fetch_optional(&mut *tx)
                        .await?;
                if exists.is_none() {
                    Some(("missing_tag", "issue does not have this tag"))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((code, error)) = state_problem {
            problems.push(IssueActionProblem {
                id: *id,
                code: code.to_string(),
                error: error.to_string(),
            });
        }
        issues.push(issue);
    }

    if !problems.is_empty() {
        tx.rollback().await?;
        return Err(IssueActionValidationError { problems }.into());
    }

    let now = now_iso();
    for issue in &issues {
        match action {
            BulkIssueAction::Close => {
                sqlx::query(
                    "UPDATE issues SET status = 'closed', closed_at = ?, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&now)
                .bind(&now)
                .bind(issue.id)
                .execute(&mut *tx)
                .await?;
            }
            BulkIssueAction::Reopen => {
                sqlx::query(
                    "UPDATE issues SET status = 'open', closed_at = NULL, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&now)
                .bind(issue.id)
                .execute(&mut *tx)
                .await?;
            }
            BulkIssueAction::Tag {
                key,
                value,
                note,
                set_by,
            } => {
                sqlx::query(
                    "INSERT INTO issue_tags (issue_id, key, value, note, set_by, set_at)
                     VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT(issue_id, key) DO UPDATE SET
                       value = excluded.value, note = excluded.note,
                       set_by = excluded.set_by, set_at = excluded.set_at",
                )
                .bind(issue.id)
                .bind(key)
                .bind(value)
                .bind(note)
                .bind(set_by)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            }
            BulkIssueAction::Untag { key } => {
                sqlx::query("DELETE FROM issue_tags WHERE issue_id = ? AND key = ?")
                    .bind(issue.id)
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            }
            BulkIssueAction::Delete => {
                sqlx::query("DELETE FROM issue_tags WHERE issue_id = ?")
                    .bind(issue.id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM issues WHERE id = ?")
                    .bind(issue.id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    let deleted_ids = if matches!(action, BulkIssueAction::Delete) {
        ids.to_vec()
    } else {
        Vec::new()
    };
    let mut updated = Vec::new();
    if deleted_ids.is_empty() {
        for id in ids {
            updated.push(
                sqlx::query_as::<_, Issue>("SELECT * FROM issues WHERE id = ?")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?,
            );
        }
    }
    tx.commit().await?;
    Ok(BulkIssueActionResult {
        issues: updated,
        deleted_ids,
    })
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Issue>> {
    let row = sqlx::query_as::<_, Issue>("SELECT * FROM issues WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

fn status_clause(include_closed: bool) -> &'static str {
    if include_closed {
        ""
    } else {
        " AND status = 'open'"
    }
}

/// Issues claimed by `branch` in `repo_root` — the branch's working set.
pub async fn list_for_branch(
    db: &Db,
    repo_root: &str,
    branch: &str,
    include_closed: bool,
) -> Result<Vec<Issue>> {
    let sql = format!(
        "SELECT * FROM issues WHERE repo_root = ? AND claimed_branch = ?{} ORDER BY id ASC",
        status_clause(include_closed)
    );
    let rows = sqlx::query_as::<_, Issue>(&sql)
        .bind(repo_root)
        .bind(branch)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Issues this branch **delegated**: created from `branch` (it is the
/// `source_branch`) but claimed by a *different* branch — i.e. the tracking
/// issues a parent agent opened when it launched sub-sessions. This is the
/// parent's view of its parallel sub-trees.
pub async fn list_delegated_by(
    db: &Db,
    repo_root: &str,
    branch: &str,
    include_closed: bool,
) -> Result<Vec<Issue>> {
    let sql = format!(
        "SELECT * FROM issues
         WHERE repo_root = ? AND source_branch = ?
           AND claimed_branch IS NOT NULL AND claimed_branch != ?{}
         ORDER BY id ASC",
        status_clause(include_closed)
    );
    let rows = sqlx::query_as::<_, Issue>(&sql)
        .bind(repo_root)
        .bind(branch)
        .bind(branch)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// The unclaimed repo backlog (`claimed_branch IS NULL`).
pub async fn list_backlog(db: &Db, repo_root: &str, include_closed: bool) -> Result<Vec<Issue>> {
    let sql = format!(
        "SELECT * FROM issues WHERE repo_root = ? AND claimed_branch IS NULL{} ORDER BY id ASC",
        status_clause(include_closed)
    );
    let rows = sqlx::query_as::<_, Issue>(&sql)
        .bind(repo_root)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Every issue in the repo, regardless of claim.
pub async fn list_for_repo(db: &Db, repo_root: &str, include_closed: bool) -> Result<Vec<Issue>> {
    let sql = format!(
        "SELECT * FROM issues WHERE repo_root = ?{} ORDER BY id ASC",
        status_clause(include_closed)
    );
    let rows = sqlx::query_as::<_, Issue>(&sql)
        .bind(repo_root)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Every issue across every repo — the loom dashboard's cross-repo issue board.
/// Ordered by repo then id so a multi-repo listing groups naturally.
pub async fn list_all(db: &Db, include_closed: bool) -> Result<Vec<Issue>> {
    let sql = format!(
        "SELECT * FROM issues WHERE 1=1{} ORDER BY repo_root ASC, id ASC",
        status_clause(include_closed)
    );
    let rows = sqlx::query_as::<_, Issue>(&sql).fetch_all(db).await?;
    Ok(rows)
}

/// Count of open issues claimed by `branch` — the per-session badge.
pub async fn open_count_for_branch(db: &Db, repo_root: &str, branch: &str) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM issues
         WHERE repo_root = ? AND claimed_branch = ? AND status = 'open'",
    )
    .bind(repo_root)
    .bind(branch)
    .fetch_one(db)
    .await?;
    Ok(n)
}

/// Count of all open issues in the repo.
pub async fn open_count_for_repo(db: &Db, repo_root: &str) -> Result<i64> {
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM issues WHERE repo_root = ? AND status = 'open'")
            .bind(repo_root)
            .fetch_one(db)
            .await?;
    Ok(n)
}

/// Set (or, with `None`, clear) the claiming branch of a single issue.
pub async fn set_claim(db: &Db, id: i64, claimed_branch: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE issues SET claimed_branch = ?, updated_at = ? WHERE id = ?")
        .bind(claimed_branch)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    match claimed_branch {
        Some(b) => tracing::info!(issue = id, branch = %b, "issue claimed"),
        None => tracing::info!(issue = id, "issue unclaimed"),
    }
    Ok(())
}

/// Release every issue claimed by `branch` back to the repo backlog. Used on
/// session teardown — the issues survive; only the claim is cleared.
pub async fn unclaim_branch(db: &Db, repo_root: &str, branch: &str) -> Result<u64> {
    let res = sqlx::query(
        "UPDATE issues SET claimed_branch = NULL, updated_at = ?
         WHERE repo_root = ? AND claimed_branch = ?",
    )
    .bind(now_iso())
    .bind(repo_root)
    .bind(branch)
    .execute(db)
    .await?;
    let n = res.rows_affected();
    if n > 0 {
        tracing::info!(repo_root, branch, count = n, "issues unclaimed");
    }
    Ok(n)
}

/// Close every open issue claimed by `branch` in `repo_root`, returning the ids
/// that were closed. Used when a session is torn down on PR merge: the work the
/// branch claimed shipped, so its tracking issues close out with it. Contrast
/// [`unclaim_branch`], which releases the claim but leaves the issue open for
/// another branch to pick up.
pub async fn close_for_branch(db: &Db, repo_root: &str, branch: &str) -> Result<Vec<i64>> {
    let now = now_iso();
    let rows: Vec<(i64,)> = sqlx::query_as(
        "UPDATE issues SET status = 'closed', closed_at = ?, updated_at = ?
         WHERE repo_root = ? AND claimed_branch = ? AND status = 'open'
         RETURNING id",
    )
    .bind(&now)
    .bind(&now)
    .bind(repo_root)
    .bind(branch)
    .fetch_all(db)
    .await?;
    let ids: Vec<i64> = rows.into_iter().map(|(id,)| id).collect();
    if !ids.is_empty() {
        tracing::info!(repo_root, branch, count = ids.len(), "issues closed");
    }
    Ok(ids)
}

pub async fn close(db: &Db, id: i64) -> Result<()> {
    let now = now_iso();
    sqlx::query("UPDATE issues SET status = 'closed', closed_at = ?, updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn reopen(db: &Db, id: i64) -> Result<()> {
    sqlx::query("UPDATE issues SET status = 'open', closed_at = NULL, updated_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> Result<()> {
    // Foreign keys aren't enabled on the pool, so the `issue_tags` cascade won't
    // fire — clear an issue's tags explicitly before removing the row.
    sqlx::query("DELETE FROM issue_tags WHERE issue_id = ?")
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM issues WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Issue tags
// ---------------------------------------------------------------------------
//
// A free-form `(key, value)` label on an issue, stored in `issue_tags` and
// shaped like a branch [`Tag`]. Unlike branch tags there is no loud
// `attention`/`triage` ladder — every issue tag is a quiet annotation
// (priority, area, kind, …). The value must be non-empty; clearing a label is
// [`clear_tag`], which deletes the row.

/// Every tag on an issue, ordered by key for a stable presentation.
pub async fn list_tags(db: &Db, issue_id: i64) -> Result<Vec<Tag>> {
    let rows = sqlx::query_as::<_, Tag>(
        "SELECT key, value, note, set_by, set_at FROM issue_tags
         WHERE issue_id = ? ORDER BY key",
    )
    .bind(issue_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Set (insert or replace) a tag on an issue. Single-valued per `(issue_id,
/// key)`: a second set for the same key overwrites the value, note, and
/// attribution and re-stamps `set_at`. The caller is expected to have validated
/// that `value` is non-empty; clearing is [`clear_tag`].
pub async fn set_tag(
    db: &Db,
    issue_id: i64,
    key: &str,
    value: &str,
    note: &str,
    set_by: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO issue_tags (issue_id, key, value, note, set_by, set_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(issue_id, key) DO UPDATE SET
           value = excluded.value, note = excluded.note,
           set_by = excluded.set_by, set_at = excluded.set_at",
    )
    .bind(issue_id)
    .bind(key)
    .bind(value)
    .bind(note)
    .bind(set_by)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

/// Clear a tag — delete the `(issue_id, key)` row. A no-op when the tag is
/// absent.
pub async fn clear_tag(db: &Db, issue_id: i64, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM issue_tags WHERE issue_id = ? AND key = ?")
        .bind(issue_id)
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A claimed issue created on `branch` in `/r`.
    fn claimed(repo: &str, branch: &str, title: &str) -> NewIssue {
        NewIssue {
            repo_root: repo.to_string(),
            source_branch: Some(branch.to_string()),
            claimed_branch: Some(branch.to_string()),
            title: title.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn lifecycle() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let i = add(&db, &claimed("/r", "feature", "fix the thing"))
            .await
            .unwrap();
        assert_eq!(i.status, "open");
        assert_eq!(i.claimed_branch.as_deref(), Some("feature"));

        let open = list_for_branch(&db, "/r", "feature", false).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(
            open_count_for_branch(&db, "/r", "feature").await.unwrap(),
            1
        );

        close(&db, i.id).await.unwrap();
        assert_eq!(
            list_for_branch(&db, "/r", "feature", false)
                .await
                .unwrap()
                .len(),
            0
        );
        let all = list_for_branch(&db, "/r", "feature", true).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "closed");

        reopen(&db, i.id).await.unwrap();
        assert_eq!(
            open_count_for_branch(&db, "/r", "feature").await.unwrap(),
            1
        );

        delete(&db, i.id).await.unwrap();
        assert_eq!(list_for_repo(&db, "/r", true).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn backlog_and_claim() {
        let db = crate::db::connect_in_memory().await.unwrap();
        // A claimed issue, plus an unclaimed backlog item authored from `main`.
        add(&db, &claimed("/r", "feature", "mine")).await.unwrap();
        let backlog_item = add(
            &db,
            &NewIssue {
                repo_root: "/r".to_string(),
                source_branch: Some("main".to_string()),
                claimed_branch: None,
                title: "pick me".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // The branch view sees only its claimed issue; the backlog sees only
        // the unclaimed one; the repo view sees both.
        assert_eq!(
            list_for_branch(&db, "/r", "feature", false)
                .await
                .unwrap()
                .len(),
            1
        );
        let backlog = list_backlog(&db, "/r", false).await.unwrap();
        assert_eq!(backlog.len(), 1);
        assert_eq!(backlog[0].id, backlog_item.id);
        assert_eq!(list_for_repo(&db, "/r", false).await.unwrap().len(), 2);

        // Claiming moves a backlog item into a branch's working set.
        set_claim(&db, backlog_item.id, Some("feature"))
            .await
            .unwrap();
        assert_eq!(list_backlog(&db, "/r", false).await.unwrap().len(), 0);
        assert_eq!(
            open_count_for_branch(&db, "/r", "feature").await.unwrap(),
            2
        );

        // Teardown releases every claim back to the backlog (issues survive).
        let released = unclaim_branch(&db, "/r", "feature").await.unwrap();
        assert_eq!(released, 2);
        assert_eq!(
            open_count_for_branch(&db, "/r", "feature").await.unwrap(),
            0
        );
        assert_eq!(list_backlog(&db, "/r", false).await.unwrap().len(), 2);
        assert_eq!(list_for_repo(&db, "/r", false).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn close_for_branch_closes_only_that_branchs_open_issues() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let a = add(&db, &claimed("/r", "feature", "task one"))
            .await
            .unwrap();
        let b = add(&db, &claimed("/r", "feature", "task two"))
            .await
            .unwrap();
        // Already closed on `feature` — must not reappear in the closed set.
        let done = add(&db, &claimed("/r", "feature", "done")).await.unwrap();
        close(&db, done.id).await.unwrap();
        // A different branch's claim is untouched.
        let other = add(&db, &claimed("/r", "other", "theirs")).await.unwrap();

        let mut closed = close_for_branch(&db, "/r", "feature").await.unwrap();
        closed.sort_unstable();
        assert_eq!(closed, vec![a.id, b.id]);
        assert_eq!(
            open_count_for_branch(&db, "/r", "feature").await.unwrap(),
            0
        );
        assert_eq!(
            get(&db, other.id).await.unwrap().unwrap().status,
            "open",
            "another branch's claim is left alone"
        );

        // Idempotent: a second call finds nothing open to close.
        assert!(close_for_branch(&db, "/r", "feature")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn delegated_lists_sub_trees_only() {
        let db = crate::db::connect_in_memory().await.unwrap();
        // `parent` delegated a task to `child` (source=parent, claimed=child).
        let delegated = add(
            &db,
            &NewIssue {
                repo_root: "/r".to_string(),
                source_branch: Some("parent".to_string()),
                claimed_branch: Some("child".to_string()),
                title: "do the sub-task".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // A self-claimed issue on `parent` is its own work, not a delegation.
        add(&db, &claimed("/r", "parent", "my own work"))
            .await
            .unwrap();

        let mine = list_delegated_by(&db, "/r", "parent", false).await.unwrap();
        assert_eq!(mine.len(), 1, "only the cross-branch issue is delegated");
        assert_eq!(mine[0].id, delegated.id);
        // The child sees nothing delegated *by* it.
        assert!(list_delegated_by(&db, "/r", "child", false)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn repos_are_isolated() {
        let db = crate::db::connect_in_memory().await.unwrap();
        add(&db, &claimed("/a", "feature", "in a")).await.unwrap();
        add(&db, &claimed("/b", "feature", "in b")).await.unwrap();
        assert_eq!(list_for_repo(&db, "/a", false).await.unwrap().len(), 1);
        assert_eq!(open_count_for_repo(&db, "/a").await.unwrap(), 1);
        // Same branch name, different repo — must not bleed across.
        assert_eq!(
            list_for_branch(&db, "/a", "feature", false)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn list_all_spans_repos() {
        let db = crate::db::connect_in_memory().await.unwrap();
        add(&db, &claimed("/a", "feature", "in a")).await.unwrap();
        add(&db, &claimed("/b", "feature", "in b")).await.unwrap();
        let closed = add(&db, &claimed("/a", "feature", "done a")).await.unwrap();
        close(&db, closed.id).await.unwrap();

        // Open-only by default, every repo, ordered repo then id.
        let open = list_all(&db, false).await.unwrap();
        assert_eq!(open.len(), 2);
        assert_eq!(open[0].repo_root, "/a");
        assert_eq!(open[1].repo_root, "/b");
        // Including closed picks up the closed `/a` issue too.
        assert_eq!(list_all(&db, true).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn tags_roundtrip_and_clear_on_delete() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let i = add(&db, &claimed("/r", "feature", "tag me")).await.unwrap();
        assert!(list_tags(&db, i.id).await.unwrap().is_empty());

        set_tag(&db, i.id, "priority", "high", "ship first", "agent")
            .await
            .unwrap();
        set_tag(&db, i.id, "area", "ui", "", "manual")
            .await
            .unwrap();
        let tags = list_tags(&db, i.id).await.unwrap();
        // Ordered by key: area, priority.
        let keys: Vec<&str> = tags.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, vec!["area", "priority"]);
        let prio = tags.iter().find(|t| t.key == "priority").unwrap();
        assert_eq!(prio.value, "high");
        assert_eq!(prio.note, "ship first");
        assert_eq!(prio.set_by, "agent");
        assert!(!prio.set_at.is_empty());

        // A second set for the same key overwrites in place.
        set_tag(&db, i.id, "priority", "low", "", "manual")
            .await
            .unwrap();
        let tags = list_tags(&db, i.id).await.unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(
            tags.iter().find(|t| t.key == "priority").unwrap().value,
            "low"
        );

        // Clearing one leaves the other.
        clear_tag(&db, i.id, "priority").await.unwrap();
        let tags = list_tags(&db, i.id).await.unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].key, "area");

        // Deleting the issue clears its remaining tags.
        delete(&db, i.id).await.unwrap();
        assert!(list_tags(&db, i.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_with_tags_persists_the_issue_and_labels_together() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let issue = add_with_tags(
            &db,
            &claimed("/r", "feature", "tagged at creation"),
            &[NewIssueTag {
                key: "priority".to_string(),
                value: "high".to_string(),
                note: "ship first".to_string(),
                set_by: "manual".to_string(),
            }],
        )
        .await
        .unwrap();

        let tags = list_tags(&db, issue.id).await.unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].key, "priority");
        assert_eq!(tags[0].value, "high");
        assert_eq!(tags[0].note, "ship first");
    }

    #[tokio::test]
    async fn create_with_tags_rolls_back_the_issue_if_a_tag_fails() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let duplicate = NewIssueTag {
            key: "priority".to_string(),
            value: "high".to_string(),
            note: String::new(),
            set_by: "manual".to_string(),
        };
        add_with_tags(
            &db,
            &claimed("/r", "feature", "must roll back"),
            &[duplicate.clone(), duplicate],
        )
        .await
        .unwrap_err();
        assert!(list_for_repo(&db, "/r", true).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn bulk_actions_cover_close_reopen_tag_untag_and_delete() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let first = add(&db, &claimed("/r", "feature", "first")).await.unwrap();
        let second = add(&db, &claimed("/r", "feature", "second")).await.unwrap();
        let ids = [first.id, second.id];

        let closed = apply_bulk_action(&db, &ids, &BulkIssueAction::Close)
            .await
            .unwrap();
        assert!(closed.issues.iter().all(|issue| issue.status == "closed"));
        let reopened = apply_bulk_action(&db, &ids, &BulkIssueAction::Reopen)
            .await
            .unwrap();
        assert!(reopened.issues.iter().all(|issue| issue.status == "open"));

        apply_bulk_action(
            &db,
            &ids,
            &BulkIssueAction::Tag {
                key: "area".to_string(),
                value: "ui".to_string(),
                note: String::new(),
                set_by: "manual".to_string(),
            },
        )
        .await
        .unwrap();
        for id in ids {
            assert_eq!(list_tags(&db, id).await.unwrap()[0].value, "ui");
        }

        apply_bulk_action(
            &db,
            &ids,
            &BulkIssueAction::Untag {
                key: "area".to_string(),
            },
        )
        .await
        .unwrap();
        for id in ids {
            assert!(list_tags(&db, id).await.unwrap().is_empty());
        }

        let deleted = apply_bulk_action(&db, &ids, &BulkIssueAction::Delete)
            .await
            .unwrap();
        assert_eq!(deleted.deleted_ids, ids);
        assert!(list_for_repo(&db, "/r", true).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn bulk_action_invalid_id_rolls_back_every_mutation() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let issue = add(&db, &claimed("/r", "feature", "valid")).await.unwrap();

        let error = apply_bulk_action(&db, &[issue.id, 999_999], &BulkIssueAction::Close)
            .await
            .unwrap_err();
        let validation = error.downcast_ref::<IssueActionValidationError>().unwrap();
        assert_eq!(
            validation.problems,
            vec![IssueActionProblem {
                id: 999_999,
                code: "not_found".to_string(),
                error: "issue not found".to_string(),
            }]
        );
        assert_eq!(get(&db, issue.id).await.unwrap().unwrap().status, "open");

        apply_bulk_action(&db, &[issue.id, 999_999], &BulkIssueAction::Delete)
            .await
            .unwrap_err();
        assert!(
            get(&db, issue.id).await.unwrap().is_some(),
            "a valid issue must survive when any delete target is invalid"
        );
    }

    #[tokio::test]
    async fn bulk_action_precondition_failure_rolls_back_every_mutation() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let open = add(&db, &claimed("/r", "feature", "open")).await.unwrap();
        let closed = add(&db, &claimed("/r", "feature", "closed")).await.unwrap();
        close(&db, closed.id).await.unwrap();

        let error = apply_bulk_action(&db, &[open.id, closed.id], &BulkIssueAction::Close)
            .await
            .unwrap_err();
        let validation = error.downcast_ref::<IssueActionValidationError>().unwrap();
        assert_eq!(validation.problems.len(), 1);
        assert_eq!(validation.problems[0].id, closed.id);
        assert_eq!(validation.problems[0].code, "invalid_state");
        assert_eq!(get(&db, open.id).await.unwrap().unwrap().status, "open");
        assert_eq!(get(&db, closed.id).await.unwrap().unwrap().status, "closed");
    }
}
