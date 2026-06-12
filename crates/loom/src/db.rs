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
    -- Per-session model tier ('', 'haiku', 'sonnet', 'opus', 'fable') and reasoning
    -- effort ('', 'low', 'medium', 'high', 'xhigh', 'max'), spliced into the
    -- Claude launch as `--model` / `--effort`. Empty inherits the global
    -- `agent.claude_args`.
    model              TEXT NOT NULL DEFAULT '',
    effort             TEXT NOT NULL DEFAULT '',
    status             TEXT NOT NULL,
    github_repo        TEXT,
    last_activity_at   TEXT,
    -- The overlooker id that owns this session when it is engine-managed
    -- infrastructure — a warm session a watcher keeps for its across-round
    -- memory. NULL for an ordinary fleet session. A managed session is hidden
    -- from the fleet listing and the survey scope, and its restart adoption is
    -- governed by `overlooker.adopt_warm`, independent of `server.auto_adopt`.
    managed_by         TEXT,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_active_branch
    ON sessions(branch_id) WHERE status NOT IN ('done', 'error');

CREATE TABLE IF NOT EXISTS recent_repos (
    repo_root    TEXT PRIMARY KEY,
    last_used_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- The latest GitHub snapshot for a branch: the pull request loom found for it
-- (via the `gh` CLI) plus its review/check rollup. One row per branch, replaced
-- on each poll; it is optional context, gone the moment the branch row is.
CREATE TABLE IF NOT EXISTS branch_github (
    branch_id        TEXT PRIMARY KEY REFERENCES branches(id) ON DELETE CASCADE,
    pr_number        INTEGER,
    pr_url           TEXT,
    -- 'OPEN' | 'CLOSED' | 'MERGED'.
    pr_state         TEXT,
    pr_title         TEXT,
    is_draft         INTEGER NOT NULL DEFAULT 0,
    -- 'APPROVED' | 'CHANGES_REQUESTED' | 'REVIEW_REQUIRED' | NULL.
    review_decision  TEXT,
    -- Rolled-up checks: 'passing' | 'failing' | 'pending' | NULL (no checks).
    checks           TEXT,
    -- 'MERGEABLE' | 'CONFLICTING' | 'UNKNOWN' | NULL.
    mergeable        TEXT,
    merged_at        TEXT,
    fetched_at       TEXT NOT NULL
);

-- Authentication (loom-only; the daemon-less `weaver` CLI never serves HTTP, so
-- it has no notion of users). An *approved* operator: a row here is the
-- allowlist. `github_login` matches the GitHub OAuth identity; `password_hash`
-- (argon2) backs username/password login. Either may be NULL — a GitHub-only
-- user has no password until they set one, and vice versa.
CREATE TABLE IF NOT EXISTS users (
    username      TEXT PRIMARY KEY,
    github_login  TEXT UNIQUE,
    password_hash TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- API tokens (personal access tokens) for automation — the `LOOM_TOKEN` a CI
-- job or the `loom` CLI presents as `Authorization: Bearer`. Only the SHA-256
-- `token_hash` is stored; the plaintext is shown once at creation. `prefix` is
-- the leading, non-secret slice kept for display ("loom_AbCd…"). `kind` is
-- 'pat' for a user token or 'local' for the machine token loom mints for its own
-- same-host subprocesses (hidden from the token list, not revocable from the UI).
CREATE TABLE IF NOT EXISTS api_tokens (
    id           TEXT PRIMARY KEY,
    username     TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    prefix       TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'pat',
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_used_at TEXT,
    expires_at   TEXT
);

-- Browser login sessions: the opaque cookie a successful GitHub/password login
-- sets. Stored hashed like a token; named `auth_sessions` to stay clear of the
-- agent `sessions` table above. A row is dropped on logout or once `expires_at`
-- passes.
CREATE TABLE IF NOT EXISTS auth_sessions (
    token_hash TEXT PRIMARY KEY,
    username   TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    expires_at TEXT NOT NULL
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
    // The branch id of the session that launched this one — its parent in the
    // dashboard's session tree. Set once at create time from the known launcher;
    // NULL for a top-level session. Sessions predating the column stay NULL (they
    // render flat, as they did before threading existed).
    add_column_if_missing(pool, "sessions", "parent_branch_id", "TEXT").await?;
    // The owning overlooker id for an engine-managed (warm) session; NULL for an
    // ordinary fleet session. Sessions predating the column stay NULL (they are
    // all ordinary fleet sessions, as they were before warm sessions existed).
    add_column_if_missing(pool, "sessions", "managed_by", "TEXT").await?;
    seed_owner(pool).await?;
    Ok(())
}

/// Seed the approved-user allowlist with the deploy owner so a fresh database is
/// usable immediately: GitHub login works for exactly this one identity until
/// more users are added. The login defaults to `rjpower` and can be overridden
/// at first run with `LOOM_OWNER_GITHUB`. `INSERT OR IGNORE` makes this a no-op
/// once the row (or any same-username row) exists, so it never clobbers later
/// edits — including a password the owner has set.
async fn seed_owner(pool: &Db) -> Result<()> {
    let owner = std::env::var("LOOM_OWNER_GITHUB")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rjpower".to_string());
    sqlx::query("INSERT OR IGNORE INTO users (username, github_login) VALUES (?, ?)")
        .bind(&owner)
        .bind(&owner)
        .execute(pool)
        .await
        .with_context(|| format!("seeding owner user '{owner}'"))?;
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
