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

use crate::db::{now_iso, Db};

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

/// Create a new issue. Returns the persisted row.
pub async fn add(db: &Db, new: &NewIssue) -> Result<Issue> {
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
    .fetch_one(db)
    .await?;
    get(db, row.0)
        .await?
        .ok_or_else(|| anyhow::anyhow!("issue vanished after insert"))
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
    Ok(res.rows_affected())
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
    sqlx::query("DELETE FROM issues WHERE id = ?")
        .bind(id)
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
}
