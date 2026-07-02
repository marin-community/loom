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
    term_session       TEXT NOT NULL,
    agent_kind         TEXT NOT NULL DEFAULT 'claude',
    -- Per-session model selector and reasoning effort, interpreted by the
    -- selected agent type. Empty uses the runtime's own default.
    model              TEXT NOT NULL DEFAULT '',
    effort             TEXT NOT NULL DEFAULT '',
    status             TEXT NOT NULL,
    github_repo        TEXT,
    last_activity_at   TEXT,
    -- The watch id that owns this session when it is engine-managed
    -- infrastructure — a warm session a watcher keeps for its across-round
    -- memory. NULL for an ordinary fleet session. A managed session is hidden
    -- from the fleet listing and the survey scope, and its restart adoption is
    -- governed by `watch.adopt_warm`, independent of `server.auto_adopt`.
    managed_by         TEXT,
    -- The principal (username) that launched this session — attribution for the
    -- shared team board. NULL for engine-created sessions (warm watch
    -- sessions) and rows that predate the column. A tracking/UX field, never a
    -- security boundary: the fleet stays co-owned, this just records who/what
    -- launched each session.
    created_by         TEXT,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_active_branch
    ON sessions(branch_id) WHERE status NOT IN ('done', 'error');

CREATE TABLE IF NOT EXISTS recent_repos (
    repo_root    TEXT PRIMARY KEY,
    last_used_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Managed repositories: GitHub repos loom has cloned (or may clone) into the
-- container-owned repo root (WEAVER_REPOS_DIR), laid out <owner>/<name>. The
-- slug -> (remote_url, path) mapping doubles as the clone allowlist: only a repo
-- registered here may be resolved and cloned for a session, the boundary the
-- GitHub trigger relies on. Distinct from `recent_repos`, which only records
-- bind-mounted local paths a session has used.
CREATE TABLE IF NOT EXISTS repos (
    slug       TEXT PRIMARY KEY,
    remote_url TEXT NOT NULL,
    path       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Processed GitHub webhook deliveries, keyed on the `X-GitHub-Delivery` GUID.
-- The receiver records each delivery before acting on it and treats a repeat as
-- a no-op, so a replayed (or GitHub-retried) delivery never launches a second
-- session. Append-only; rows are cheap and can be pruned by age later.
CREATE TABLE IF NOT EXISTS processed_deliveries (
    delivery_id TEXT PRIMARY KEY,
    received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
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
    username       TEXT PRIMARY KEY,
    github_login   TEXT UNIQUE,
    password_hash  TEXT,
    -- Captured at GitHub sign-in for commit attribution (design §6.3, Level A).
    -- `github_user_id` yields the stable `<id>+<login>@users.noreply.github.com`
    -- commit email that links a commit to the GitHub account; `display_name` is
    -- the profile name used for the git author name. Both NULL until the user has
    -- signed in via GitHub since these columns existed.
    github_user_id INTEGER,
    display_name   TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
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

-- Operator-managed environment variables exported into every interactive agent
-- session loom launches (alongside loom's own WEAVER_* / LOOM_TOKEN). A plain
-- name/value store edited at runtime from the settings pane, so secrets and
-- tool config (e.g. a registry token, GH_HOST) can be added without rebuilding
-- the image or editing the deploy env file. NOT applied to the env-stripped
-- one-shot judgement agent.
CREATE TABLE IF NOT EXISTS agent_env (
    name       TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Per-repo environment variables, layered into a session's agent terminal above
-- the global `agent_env` (global < per-repo < the repo's own .weaver/config.toml
-- [env]). Keyed by the canonical `repo_root` path, like `branches`/`issues`, so a
-- launch (which has the resolved repo root) can look them up directly. Values are
-- write-only: the API returns names + timestamps, never the value, since these
-- hold per-repo secrets (registry tokens, database URLs). Blast-radius reduction,
-- not isolation — in the single shared container any agent can still read the
-- exported env (see the shared-loom design §6.4).
CREATE TABLE IF NOT EXISTS repo_env (
    repo_root  TEXT NOT NULL,
    name       TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (repo_root, name)
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

-- A user's own GitHub token (a fine-grained PAT they paste into their account
-- pane), injected as GH_TOKEN into the interactive sessions that user launches
-- so an agent's `git push` / `gh` acts as *them* rather than as the shared
-- ambient GH_TOKEN from the deploy env. Write-only over the API (status +
-- timestamp, never the token), like `repo_env` — blast-radius reduction, not
-- isolation (any agent in the shared container can still read the exported
-- GH_TOKEN; see the shared-loom design §6.4). One row per user; dropped with the
-- user via ON DELETE CASCADE.
CREATE TABLE IF NOT EXISTS user_github_tokens (
    username   TEXT PRIMARY KEY REFERENCES users(username) ON DELETE CASCADE,
    token      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Operator-defined custom agents: a coding agent the user wires up by naming the
-- shell commands loom runs at each launch stage, so it shows up in the agent list
-- beside the builtin `claude`/`codex` without a code change. `name` is the id the
-- agent list and a session's `agent_kind` reference; the builtin names are
-- reserved (see `custom_agents::validate_name`). Each stage is a shell fragment:
--   * `setup`  — run in the worktree before launch (e.g. installing status hooks);
--   * `launch` — the fresh-session command, with the goal appended as an argument;
--   * `resume` — the adopt/resume command (blank falls back to `launch`).
-- `reports_status` records whether the agent fires weaver's lifecycle hooks, which
-- drive the idle/attention signals (a fresh session is `running` immediately).
CREATE TABLE IF NOT EXISTS custom_agents (
    name           TEXT PRIMARY KEY,
    label          TEXT NOT NULL,
    setup          TEXT NOT NULL DEFAULT '',
    launch         TEXT NOT NULL DEFAULT '',
    resume         TEXT NOT NULL DEFAULT '',
    reports_status INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
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
    // Rename for databases created when the column was `tmux_session` (loom
    // backed sessions with tmux before the native terminal supervisor). The
    // `CREATE TABLE IF NOT EXISTS` above is a no-op on such DBs, so rename the
    // existing column; a fresh DB already has `term_session`, so the rename finds
    // nothing and is ignored.
    rename_column_if_present(pool, "sessions", "tmux_session", "term_session").await?;
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
    // The owning watch id for an engine-managed (warm) session; NULL for an
    // ordinary fleet session. Sessions predating the column stay NULL (they are
    // all ordinary fleet sessions, as they were before warm sessions existed).
    add_column_if_missing(pool, "sessions", "managed_by", "TEXT").await?;
    // The principal (username) that launched this session — attribution for the
    // shared team board. NULL for engine-created sessions and rows predating the
    // column. (The `sessions` table is created here in `migrate_loom`, after the
    // weaver-core numbered migrations have already run, so a `sessions` column is
    // added here rather than as a numbered migration in weaver-core/migrations —
    // matching `model`/`effort`/`parent_branch_id`/`managed_by` above.)
    add_column_if_missing(pool, "sessions", "created_by", "TEXT").await?;
    // GitHub profile captured at sign-in for commit attribution (see the `users`
    // schema above). Added here for databases predating the columns; a fresh DB
    // already has them from the `CREATE TABLE` and these are no-ops.
    add_column_if_missing(pool, "users", "github_user_id", "INTEGER").await?;
    add_column_if_missing(pool, "users", "display_name", "TEXT").await?;
    seed_owner(pool).await?;
    Ok(())
}

/// Seed the approved-user allowlist with the deploy owner named by
/// `LOOM_OWNER_GITHUB`, so a fresh database is usable immediately: GitHub login
/// works for exactly this one identity until more users are added.
/// `INSERT OR IGNORE` makes this a no-op once the row (or any same-username
/// row) exists, so it never clobbers later edits — including a password the
/// owner has set.
///
/// Fails closed: with `LOOM_OWNER_GITHUB` unset or empty, no owner is seeded at
/// all — a warning is logged and GitHub/loopback login simply has no `users`
/// row to resolve to until the operator sets it (see [`crate::auth::primary_user`]
/// returning `None`). This never falls back to a real login (e.g. the
/// maintainer's own), which would hand an internet-facing deploy's sole owner
/// slot to someone other than its operator.
async fn seed_owner(pool: &Db) -> Result<()> {
    let owner = std::env::var("LOOM_OWNER_GITHUB")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(owner) = owner else {
        tracing::warn!(
            "LOOM_OWNER_GITHUB is not set — no owner user was seeded. No one can log in until \
             you set LOOM_OWNER_GITHUB and restart the daemon (seeding re-runs on every start, \
             so no fresh migration is needed)."
        );
        return Ok(());
    };
    sqlx::query("INSERT OR IGNORE INTO users (username, github_login) VALUES (?, ?)")
        .bind(&owner)
        .bind(&owner)
        .execute(pool)
        .await
        .with_context(|| format!("seeding owner user '{owner}'"))?;
    Ok(())
}

/// Run `ALTER TABLE … RENAME COLUMN from TO to`, treating a missing source
/// column as success (a fresh DB already has the new name, so there is nothing
/// to rename).
async fn rename_column_if_present(pool: &Db, table: &str, from: &str, to: &str) -> Result<()> {
    let sql = format!("ALTER TABLE {table} RENAME COLUMN {from} TO {to}");
    match sqlx::query(&sql).execute(pool).await {
        Ok(_) => {
            tracing::info!(%table, %from, %to, "renamed column");
            Ok(())
        }
        // SQLite: "no such column: <from>" once already renamed / fresh schema.
        Err(e) if e.to_string().contains("no such column") => Ok(()),
        Err(e) => Err(e).with_context(|| format!("renaming column {table}.{from} -> {to}")),
    }
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
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
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
