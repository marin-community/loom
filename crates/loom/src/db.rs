//! Loom database setup: opens the shared database through `weaver-core`, then
//! applies loom's independently versioned, loom-owned schema.

use anyhow::{Context, Result};
use std::path::Path;

pub use weaver_core::db::{
    connect_in_memory as core_connect_in_memory, default_db_path, now_iso, run_dir, weaver_home, Db,
};
use weaver_core::migrations::{add_column_if_missing, split_statements, table_columns, Stream};

const LOOM_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "baseline",
    include_str!("../migrations/0001_baseline.sql"),
)];

const LOOM_STREAM: Stream = Stream {
    indicator_table: "loom_schema_migrations",
    migrations: LOOM_MIGRATIONS,
};

/// Open the shared database and apply loom's schema after the core schema.
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
    let indicator_exists = LOOM_STREAM.indicator_exists(pool).await?;
    let baseline_recorded = if indicator_exists {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM loom_schema_migrations WHERE version = 1)",
        )
        .fetch_one(pool)
        .await
        .context("checking loom baseline version")?
    } else {
        false
    };

    // Loom tables predate their migration stream. If an unversioned sessions
    // table is present, bring every supported historical shape to the baseline
    // end state and stamp it rather than replaying baseline DDL against it. Test
    // the version, not just the indicator table: a crash after creating the
    // indicator but before stamping must resume adoption on the next start.
    if !baseline_recorded && !table_columns(pool, "sessions").await?.is_empty() {
        adopt_unversioned_schema(pool).await?;
        LOOM_STREAM.ensure_indicator(pool).await?;
        LOOM_STREAM.stamp(pool, 1, "baseline").await?;
    }

    LOOM_STREAM.ensure_indicator(pool).await?;
    LOOM_STREAM.apply_pending(pool).await?;

    // Configuration-dependent seeding is intentionally not a migration. If the
    // first start has no owner configured, a later restart must retry it.
    seed_owner(pool).await?;
    Ok(())
}

/// Adopt a database built by the pre-stream `LOOM_SCHEMA` runner.
///
/// Every operation is presence-gated or idempotent. The function can therefore
/// resume after any interrupted statement, and it never compares an existing
/// column's exact declaration: deployed databases legitimately carry stricter
/// declarations than some historical source revisions.
async fn adopt_unversioned_schema(pool: &Db) -> Result<()> {
    tracing::info!("adopting unversioned loom schema");

    // Create any loom tables that were introduced after this database was
    // first deployed. Existing tables are left untouched and reconciled below.
    for stmt in split_statements(LOOM_MIGRATIONS[0].2) {
        sqlx::query(&stmt)
            .execute(pool)
            .await
            .with_context(|| format!("adopting loom baseline: {stmt}"))?;
    }

    rename_session_terminal_column(pool).await?;
    for (column, declaration) in [
        ("model", "TEXT NOT NULL DEFAULT ''"),
        ("effort", "TEXT NOT NULL DEFAULT ''"),
        ("parent_branch_id", "TEXT"),
        ("managed_by", "TEXT"),
        ("created_by", "TEXT"),
        ("park", "TEXT"),
        ("sort_order", "REAL"),
        ("protocol", "TEXT NOT NULL DEFAULT 'terminal'"),
        ("acp_session_id", "TEXT"),
        ("acp_ack_seq", "INTEGER NOT NULL DEFAULT 0"),
        ("acp_inflight", "TEXT"),
        ("current_mode", "TEXT"),
        ("pending_prompt", "TEXT NOT NULL DEFAULT ''"),
        ("origin", "TEXT NOT NULL DEFAULT 'user'"),
        ("class", "TEXT NOT NULL DEFAULT 'interactive'"),
        ("turn_count", "INTEGER NOT NULL DEFAULT 0"),
        ("tracking_issue_id", "INTEGER"),
    ] {
        add_column_if_missing(pool, "sessions", column, declaration).await?;
    }

    add_column_if_missing(
        pool,
        "custom_agents",
        "protocol",
        "TEXT NOT NULL DEFAULT 'terminal'",
    )
    .await?;
    add_column_if_missing(pool, "users", "github_user_id", "INTEGER").await?;
    add_column_if_missing(pool, "users", "display_name", "TEXT").await?;

    // These are safe to repeat. In particular, repeating them repairs a crash
    // after `origin` or `class` was added but before the old one-shot backfill.
    sqlx::query(
        "UPDATE sessions SET origin = 'watch', class = 'automation'
         WHERE managed_by IS NOT NULL",
    )
    .execute(pool)
    .await
    .context("backfilling watch session provenance")?;
    sqlx::query(
        "UPDATE sessions SET origin = 'agent'
         WHERE origin = 'user' AND parent_branch_id IS NOT NULL",
    )
    .execute(pool)
    .await
    .context("backfilling delegated session provenance")?;

    // `CREATE INDEX IF NOT EXISTS` retains an older predicate. Rebuild it after
    // session adoption so archived history no longer occupies the branch slot.
    sqlx::query("DROP INDEX IF EXISTS idx_sessions_active_branch")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX idx_sessions_active_branch
         ON sessions(branch_id) WHERE status NOT IN ('done', 'error', 'archived')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Reconcile the one historical rename without inspecting error strings.
async fn rename_session_terminal_column(pool: &Db) -> Result<()> {
    let columns = table_columns(pool, "sessions").await?;
    let has_old = columns.iter().any(|column| column == "tmux_session");
    let has_new = columns.iter().any(|column| column == "term_session");
    match (has_old, has_new) {
        (true, false) => {
            sqlx::query("ALTER TABLE sessions RENAME COLUMN tmux_session TO term_session")
                .execute(pool)
                .await
                .context("renaming sessions.tmux_session to term_session")?;
        }
        // A partially/manual-upgraded database can contain both. Preserve the
        // new column as authoritative, fill only empty values from the old,
        // then remove the legacy NOT NULL column: leaving it behind would make
        // every later insert that supplies only `term_session` fail.
        (true, true) => {
            sqlx::query(
                "UPDATE sessions SET term_session = tmux_session
                 WHERE term_session IS NULL OR term_session = ''",
            )
            .execute(pool)
            .await
            .context("reconciling sessions terminal columns")?;
            sqlx::query("ALTER TABLE sessions DROP COLUMN tmux_session")
                .execute(pool)
                .await
                .context("dropping legacy sessions.tmux_session column")?;
        }
        (false, true) => {}
        (false, false) => {
            anyhow::bail!("unversioned sessions table has neither term_session nor tmux_session")
        }
    }
    Ok(())
}

/// Seed the approved-user allowlist with the configured deploy owner.
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
    sqlx::query(
        "INSERT INTO users (username, github_login) VALUES (?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(&owner)
    .bind(&owner)
    .execute(pool)
    .await
    .with_context(|| format!("seeding owner user '{owner}'"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    async fn insert_branch(db: &Db, id: &str) {
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch, created_at, updated_at)
             VALUES (?, '/r', ?, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(id)
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn fresh_schema_records_baseline_and_has_final_shape() {
        let db = connect_in_memory().await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM loom_schema_migrations ORDER BY version")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(versions, vec![1]);

        let columns = table_columns(&db, "sessions").await.unwrap();
        for expected in [
            "term_session",
            "parent_branch_id",
            "protocol",
            "pending_prompt",
            "origin",
            "class",
            "turn_count",
            "tracking_issue_id",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }

        insert_branch(&db, "t1").await;
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
             VALUES ('archived', 't1', '/w1', 'term-1', 'archived'),
                    ('active', 't1', '/w2', 'term-2', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn adopts_old_terminal_name_and_preserves_rows() {
        let db = core_connect_in_memory().await.unwrap();
        insert_branch(&db, "b1").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                branch_id TEXT NOT NULL,
                work_dir TEXT NOT NULL,
                tmux_session TEXT NOT NULL,
                agent_kind TEXT NOT NULL DEFAULT 'claude',
                status TEXT NOT NULL,
                github_repo TEXT,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, tmux_session, status, created_at)
             VALUES ('s1', 'b1', '/w', 'legacy-term', 'running', '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();

        migrate_loom(&db).await.unwrap();
        migrate_loom(&db).await.unwrap();

        let columns = table_columns(&db, "sessions").await.unwrap();
        assert!(columns.iter().any(|column| column == "term_session"));
        assert!(!columns.iter().any(|column| column == "tmux_session"));
        let row = sqlx::query("SELECT term_session, protocol, origin, class FROM sessions")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("term_session"), "legacy-term");
        assert_eq!(row.get::<String, _>("protocol"), "terminal");
        assert_eq!(row.get::<String, _>("origin"), "user");
        assert_eq!(row.get::<String, _>("class"), "interactive");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM loom_schema_migrations")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 1);

        // Adoption replaced the historical index predicate: archived history
        // no longer prevents a new active session from claiming the branch.
        sqlx::query("UPDATE sessions SET status = 'archived' WHERE id = 's1'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
             VALUES ('s2', 'b1', '/w2', 'new-term', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn partial_provenance_upgrade_is_repaired_idempotently() {
        let db = core_connect_in_memory().await.unwrap();
        insert_branch(&db, "watch-branch").await;
        insert_branch(&db, "agent-branch").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                branch_id TEXT NOT NULL,
                work_dir TEXT NOT NULL,
                term_session TEXT NOT NULL,
                agent_kind TEXT NOT NULL DEFAULT 'claude',
                status TEXT NOT NULL,
                github_repo TEXT,
                last_activity_at TEXT,
                parent_branch_id TEXT,
                managed_by TEXT,
                origin TEXT NOT NULL DEFAULT 'user',
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, managed_by, created_at)
             VALUES ('watch', 'watch-branch', '/w', 't1', 'running', 'watch-1', '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, parent_branch_id, created_at)
             VALUES ('agent', 'agent-branch', '/a', 't2', 'running', 'parent', '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();

        // Model a crash after the indicator table was created but before the
        // legacy baseline was stamped.
        LOOM_STREAM.ensure_indicator(&db).await.unwrap();
        migrate_loom(&db).await.unwrap();
        migrate_loom(&db).await.unwrap();

        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, origin, class FROM sessions ORDER BY id")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![
                ("agent".into(), "agent".into(), "interactive".into()),
                ("watch".into(), "watch".into(), "automation".into()),
            ]
        );
    }

    #[tokio::test]
    async fn adoption_reconciles_both_terminal_columns() {
        let db = core_connect_in_memory().await.unwrap();
        insert_branch(&db, "b1").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, branch_id TEXT NOT NULL, work_dir TEXT NOT NULL,
                tmux_session TEXT NOT NULL, term_session TEXT,
                agent_kind TEXT NOT NULL DEFAULT 'claude', status TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, tmux_session, term_session, status)
             VALUES ('s1', 'b1', '/w', 'old-value', '', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();

        migrate_loom(&db).await.unwrap();
        let value: String = sqlx::query_scalar("SELECT term_session FROM sessions WHERE id = 's1'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(value, "old-value");

        let columns = table_columns(&db, "sessions").await.unwrap();
        assert!(!columns.iter().any(|column| column == "tmux_session"));
        insert_branch(&db, "b2").await;
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
             VALUES ('s2', 'b2', '/w2', 'new-value', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
    }
}
