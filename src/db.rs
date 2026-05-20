use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub type Db = SqlitePool;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workspaces (
    id                 TEXT PRIMARY KEY,
    name               TEXT NOT NULL,
    title              TEXT NOT NULL DEFAULT '',
    goal               TEXT NOT NULL DEFAULT '',
    description        TEXT NOT NULL DEFAULT '',
    status             TEXT NOT NULL DEFAULT 'created',
    repo_root          TEXT NOT NULL,
    work_dir           TEXT NOT NULL,
    branch             TEXT NOT NULL,
    base_branch        TEXT NOT NULL DEFAULT 'main',
    tmux_session       TEXT NOT NULL,
    agent_kind         TEXT NOT NULL DEFAULT 'claude',
    github_repo        TEXT,
    github_issue       INTEGER,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_activity_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    summary_updated_at TEXT,
    pending_prompt     TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_workspaces_status ON workspaces(status);

CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL,
    kind         TEXT NOT NULL,
    data         TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_events_ws ON events(workspace_id, id);

CREATE TABLE IF NOT EXISTS summaries (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id  TEXT NOT NULL,
    description   TEXT NOT NULL,
    files_changed INTEGER NOT NULL DEFAULT 0,
    insertions    INTEGER NOT NULL DEFAULT 0,
    deletions     INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_summaries_ws ON summaries(workspace_id, id);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Repositories the user has started a workspace in. Unlike `workspaces`, a
-- row here outlives the removal of all of a repo's workspaces, so the web UI
-- can keep offering recently-used repos when starting a new session.
CREATE TABLE IF NOT EXISTS recent_repos (
    repo_root    TEXT PRIMARY KEY,
    last_used_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
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

/// Directory holding a workspace's runtime files (e.g. the goal file).
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
    let statements: Vec<&str> = SCHEMA
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    tracing::info!(statements = statements.len(), "applying schema");
    for trimmed in &statements {
        tracing::debug!(statement = %trimmed, "running migration");
        sqlx::query(trimmed)
            .execute(pool)
            .await
            .with_context(|| format!("running migration: {trimmed}"))?;
    }
    // Idempotent column additions for databases created by older versions.
    // A "duplicate column" error on a fresh database is expected and ignored.
    const ALTERS: &[&str] = &[
        "ALTER TABLE workspaces ADD COLUMN title TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE workspaces ADD COLUMN pending_prompt TEXT NOT NULL DEFAULT ''",
    ];
    for stmt in ALTERS {
        if let Err(e) = sqlx::query(stmt).execute(pool).await {
            // Expected on a fresh database (column already exists). Logged at
            // debug so a genuinely unexpected failure is still visible.
            tracing::debug!(error = %e, statement = %stmt, "idempotent alter skipped");
        }
    }
    // Backfill `recent_repos` from any pre-existing workspaces so the feature
    // works on databases created before the table existed. `INSERT OR IGNORE`
    // makes this idempotent: it never overwrites a repo's recorded recency.
    sqlx::query(
        "INSERT OR IGNORE INTO recent_repos (repo_root, last_used_at)
         SELECT repo_root, MAX(created_at) FROM workspaces GROUP BY repo_root",
    )
    .execute(pool)
    .await
    .context("seeding recent_repos from existing workspaces")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_schema_works() {
        let db = connect_in_memory().await.unwrap();
        sqlx::query("INSERT INTO workspaces (id,name,repo_root,work_dir,branch,tmux_session) VALUES ('t','t','/r','/w','b','s')")
            .execute(&db)
            .await
            .unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM workspaces")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }
}
