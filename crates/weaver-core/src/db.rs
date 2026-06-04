use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub type Db = SqlitePool;

const SCHEMA: &str = r#"
-- A branch the agent is working on. Identified by `(repo_root, branch)`; the
-- 8-char `id` is internal — agents never see it.
CREATE TABLE IF NOT EXISTS branches (
    id           TEXT PRIMARY KEY,
    repo_root    TEXT NOT NULL,
    branch       TEXT NOT NULL,
    base_branch  TEXT NOT NULL DEFAULT 'main',
    goal         TEXT NOT NULL DEFAULT '',
    title        TEXT NOT NULL DEFAULT '',
    description  TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE(repo_root, branch)
);

-- Issues belong to a **repo** (`repo_root`), not a branch. The branch they were
-- created from (`source_branch`) and the branch currently working them
-- (`claimed_branch`) are annotations: `claimed_branch IS NULL` is the unclaimed
-- repo backlog. Repo-owned means an issue outlives the branch/worktree that
-- spawned it — see docs/repo-scoped-issues.md.
CREATE TABLE IF NOT EXISTS issues (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_root      TEXT NOT NULL,
    github_repo    TEXT,
    source_branch  TEXT,
    claimed_branch TEXT,
    title          TEXT NOT NULL,
    body           TEXT NOT NULL DEFAULT '',
    status         TEXT NOT NULL DEFAULT 'open',
    github_issue   INTEGER,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    closed_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_issues_repo ON issues(repo_root, status);
CREATE INDEX IF NOT EXISTS idx_issues_claimed ON issues(repo_root, claimed_branch);

CREATE TABLE IF NOT EXISTS notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id   TEXT NOT NULL REFERENCES branches(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id   TEXT NOT NULL,
    kind        TEXT NOT NULL,
    data        TEXT NOT NULL DEFAULT '{}',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_events_branch ON events(branch_id, id);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
"#;

/// Root directory for all weaver state on this machine.
pub fn weaver_home() -> PathBuf {
    if let Ok(p) = std::env::var("WEAVER_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".weaver")
}

/// Path to the single per-VM SQLite database.
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("WEAVER_DB") {
        return PathBuf::from(p);
    }
    weaver_home().join("weaver.db")
}

/// Directory holding a session's runtime files (e.g. the goal file).
pub fn run_dir(id: &str) -> PathBuf {
    weaver_home().join("run").join(id)
}

/// Current UTC time as an ISO-8601 string, matching the SQLite default format.
pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub async fn connect(path: &Path) -> Result<Db> {
    tracing::info!(path = %path.display(), "opening database");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db directory {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
        .with_context(|| format!("invalid database path {}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;
    migrate(&pool).await?;
    tracing::info!(path = %path.display(), "database ready");
    Ok(pool)
}

pub async fn connect_in_memory() -> Result<Db> {
    tracing::info!("opening in-memory database");
    let options = SqliteConnectOptions::new()
        .in_memory(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &Db) -> Result<()> {
    // Phase 2 is a clean break: any legacy `workspaces`-shaped database is
    // dropped wholesale before the new schema is applied. Old data is gone by
    // design (the user OK'd this). Detect "legacy" by the presence of a
    // `workspaces` table; once that's gone, the drops never run again.
    let legacy: Option<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name='workspaces'")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if legacy.is_some() {
        tracing::warn!("dropping legacy workspaces/events/summaries tables for the new schema");
        const DROPS: &[&str] = &[
            "DROP TABLE IF EXISTS workspaces",
            // Old events/summaries pointed at workspace_id; the new tables use
            // branch_id / session_id. Drop them so the CREATE below succeeds.
            "DROP TABLE IF EXISTS events",
            "DROP TABLE IF EXISTS summaries",
        ];
        for stmt in DROPS {
            if let Err(e) = sqlx::query(stmt).execute(pool).await {
                tracing::debug!(error = %e, statement = stmt, "drop skipped");
            }
        }
    }

    // Reshape a pre-repo-scope `issues` table (keyed on `branch_id`) into the
    // repo-owned shape before the CREATEs below would no-op over it.
    migrate_issues_to_repo_scope(pool).await?;

    // Strip `--` line comments before splitting — semicolons inside `--`
    // comments would otherwise produce truncated statements.
    let stripped: String = SCHEMA
        .lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<&str>>()
        .join("\n");
    let statements: Vec<String> = stripped
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    tracing::info!(statements = statements.len(), "applying schema");
    for trimmed in &statements {
        tracing::debug!(statement = %trimmed, "running migration");
        sqlx::query(trimmed.as_str())
            .execute(pool)
            .await
            .with_context(|| format!("running migration: {trimmed}"))?;
    }
    Ok(())
}

/// Column names of `table`, or empty if it doesn't exist.
async fn table_columns(pool: &Db, table: &str) -> Result<Vec<String>> {
    // `table` is always a hardcoded literal here, so the format! is safe.
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(|r| r.get::<String, _>("name")).collect())
}

/// One-way migration from the legacy per-branch `issues` table (a `branch_id`
/// FK with `ON DELETE CASCADE`) to the repo-owned shape. Backfills `repo_root`
/// and the `source_branch` / `claimed_branch` annotations from the owning
/// branch — which were 1:1 with the issue, so the post-migration branch view
/// reproduces the old per-branch list exactly. No-op on a fresh or
/// already-migrated database.
async fn migrate_issues_to_repo_scope(pool: &Db) -> Result<()> {
    let cols = table_columns(pool, "issues").await?;
    // Fresh db (table created below by SCHEMA) or already migrated.
    if cols.is_empty() || cols.iter().any(|c| c == "repo_root") {
        return Ok(());
    }
    // Unknown shape we don't recognize — leave it untouched.
    if !cols.iter().any(|c| c == "branch_id") {
        return Ok(());
    }
    tracing::warn!("migrating issues from per-branch to repo-scoped");
    let mut tx = pool.begin().await?;
    sqlx::query(
        "CREATE TABLE issues_new (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            repo_root      TEXT NOT NULL,
            github_repo    TEXT,
            source_branch  TEXT,
            claimed_branch TEXT,
            title          TEXT NOT NULL,
            body           TEXT NOT NULL DEFAULT '',
            status         TEXT NOT NULL DEFAULT 'open',
            github_issue   INTEGER,
            created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            closed_at      TEXT
        )",
    )
    .execute(&mut *tx)
    .await?;
    // INNER JOIN: under the old CASCADE no issue could outlive its branch, so
    // every row matches; any stray orphan is intentionally dropped.
    sqlx::query(
        "INSERT INTO issues_new
            (id, repo_root, github_repo, source_branch, claimed_branch,
             title, body, status, github_issue, created_at, updated_at, closed_at)
         SELECT i.id, b.repo_root, NULL, b.branch, b.branch,
                i.title, i.body, i.status, i.github_issue, i.created_at, i.updated_at, i.closed_at
         FROM issues i JOIN branches b ON i.branch_id = b.id",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DROP TABLE issues").execute(&mut *tx).await?;
    sqlx::query("ALTER TABLE issues_new RENAME TO issues")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_schema_works() {
        let db = connect_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch, created_at, updated_at)
             VALUES ('t1', '/r', 'main', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .execute(&db)
        .await
        .unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM branches")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn migrates_legacy_issues_to_repo_scope() {
        // Build a legacy-shaped database by hand: a `branches` table plus the
        // old per-branch `issues` table keyed on `branch_id`.
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE branches (id TEXT PRIMARY KEY, repo_root TEXT NOT NULL,
             branch TEXT NOT NULL, base_branch TEXT NOT NULL DEFAULT 'main')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE issues (id INTEGER PRIMARY KEY AUTOINCREMENT,
             branch_id TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL DEFAULT 'open', github_issue INTEGER,
             created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '',
             closed_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO branches (id, repo_root, branch) VALUES ('b1', '/repo', 'feature')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO issues (branch_id, title, status) VALUES ('b1', 'old issue', 'open')")
            .execute(&pool)
            .await
            .unwrap();

        migrate_issues_to_repo_scope(&pool).await.unwrap();

        let cols = table_columns(&pool, "issues").await.unwrap();
        assert!(cols.iter().any(|c| c == "repo_root"));
        assert!(!cols.iter().any(|c| c == "branch_id"));
        let row = sqlx::query(
            "SELECT repo_root, source_branch, claimed_branch, title FROM issues WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("repo_root"), "/repo");
        assert_eq!(row.get::<String, _>("source_branch"), "feature");
        assert_eq!(row.get::<String, _>("claimed_branch"), "feature");
        assert_eq!(row.get::<String, _>("title"), "old issue");

        // Idempotent: a second pass detects the new shape and does nothing.
        migrate_issues_to_repo_scope(&pool).await.unwrap();
        assert_eq!(table_columns(&pool, "issues").await.unwrap(), cols);
    }
}
