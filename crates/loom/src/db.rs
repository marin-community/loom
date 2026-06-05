//! Loom database setup: opens the shared `~/.weaver/weaver.db` via
//! `weaver-core`, then adds loom-owned tables (`sessions`, `recent_repos`) on
//! top.

use anyhow::{Context, Result};
use std::path::Path;

pub use weaver_core::db::{
    connect_in_memory as core_connect_in_memory, default_db_path, now_iso, run_dir, weaver_home, Db,
};

const LOOM_SCHEMA: &str = r#"
-- One *active* session per branch, enforced by the partial unique index
-- below; terminal sessions stay in the table for history.
CREATE TABLE IF NOT EXISTS sessions (
    id                 TEXT PRIMARY KEY,
    branch_id          TEXT NOT NULL REFERENCES branches(id) ON DELETE CASCADE,
    work_dir           TEXT NOT NULL,
    tmux_session       TEXT NOT NULL,
    agent_kind         TEXT NOT NULL DEFAULT 'claude',
    -- Per-session model tier ('', 'haiku', 'sonnet', 'opus') and reasoning
    -- effort ('', 'low', 'medium', 'high', 'xhigh', 'max'), spliced into the
    -- Claude launch as `--model` / `--effort`. Empty inherits the global
    -- `agent.claude_args`.
    model              TEXT NOT NULL DEFAULT '',
    effort             TEXT NOT NULL DEFAULT '',
    status             TEXT NOT NULL,
    github_repo        TEXT,
    last_activity_at   TEXT,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_active_branch
    ON sessions(branch_id) WHERE status NOT IN ('done', 'error');

CREATE TABLE IF NOT EXISTS recent_repos (
    repo_root    TEXT PRIMARY KEY,
    last_used_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
"#;

/// Open the shared database and apply loom's additional tables on top of the
/// core schema.
pub async fn connect(path: &Path) -> Result<Db> {
    let pool = weaver_core::db::connect(path).await?;
    migrate_loom(&pool).await?;
    Ok(pool)
}

/// In-memory variant for tests.
pub async fn connect_in_memory() -> Result<Db> {
    let pool = weaver_core::db::connect_in_memory().await?;
    migrate_loom(&pool).await?;
    Ok(pool)
}

async fn migrate_loom(pool: &Db) -> Result<()> {
    let stripped: String = LOOM_SCHEMA
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
    tracing::info!(statements = statements.len(), "applying loom schema");
    for trimmed in &statements {
        tracing::debug!(statement = %trimmed, "running loom migration");
        sqlx::query(trimmed.as_str())
            .execute(pool)
            .await
            .with_context(|| format!("running loom migration: {trimmed}"))?;
    }
    // Additive column migrations for databases created before the column
    // existed. `CREATE TABLE IF NOT EXISTS` above is a no-op on such DBs, so we
    // add the column explicitly and ignore the "duplicate column" error.
    add_column_if_missing(pool, "sessions", "model", "TEXT NOT NULL DEFAULT ''").await?;
    add_column_if_missing(pool, "sessions", "effort", "TEXT NOT NULL DEFAULT ''").await?;
    Ok(())
}

/// Run `ALTER TABLE … ADD COLUMN`, treating an already-present column as success.
async fn add_column_if_missing(pool: &Db, table: &str, column: &str, decl: &str) -> Result<()> {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {decl}");
    match sqlx::query(&sql).execute(pool).await {
        Ok(_) => {
            tracing::info!(%table, %column, "added column");
            Ok(())
        }
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e).with_context(|| format!("adding column {table}.{column}")),
    }
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
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, tmux_session, status)
             VALUES ('s1', 't1', '/w', 'weaver-s1', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }
}
