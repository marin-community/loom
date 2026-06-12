//! Schema migrations.
//!
//! Ordered, versioned SQL migrations applied at startup and recorded in a
//! `schema_migrations` indicator table so each one runs exactly once. The
//! migration files live alongside this crate under `migrations/` and are
//! embedded at compile time (the `weaver` binary ships without its source
//! tree), one numbered `.sql` file per migration.
//!
//! Adding a migration: drop a new `NNNN_name.sql` into `migrations/` and append
//! a `(version, name, include_str!)` row to [`MIGRATIONS`]. Never edit a
//! migration that has shipped — write a new one.
//!
//! Databases that predate this framework have no `schema_migrations` table. The
//! first time the runner sees such a database it performs a one-time
//! [`legacy_bootstrap`] (drop the ancient `workspaces` tables, reshape the
//! pre-repo-scope `issues` table, backfill additive columns) to bring it to the
//! point where the baseline migration applies cleanly, then records every
//! migration as it runs.

use anyhow::{Context, Result};
use sqlx::Row;

use crate::db::{now_iso, Db};

/// The ordered migration set: `(version, name, sql)`. Versions are dense and
/// strictly increasing; the runner applies any not yet recorded, in order.
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "baseline",
        include_str!("../migrations/0001_baseline.sql"),
    ),
    (
        2,
        "drop_notes",
        include_str!("../migrations/0002_drop_notes.sql"),
    ),
    (3, "triage", include_str!("../migrations/0003_triage.sql")),
    (
        4,
        "overlookers",
        include_str!("../migrations/0004_overlookers.sql"),
    ),
    (5, "tags", include_str!("../migrations/0005_tags.sql")),
    (
        6,
        "issue_tags",
        include_str!("../migrations/0006_issue_tags.sql"),
    ),
];

/// Apply every pending migration, bringing the database up to the latest schema.
pub async fn run(pool: &Db) -> Result<()> {
    // A database without the indicator table is either brand-new or predates
    // the framework; bootstrap it once before the ordered migrations run.
    if !indicator_exists(pool).await? {
        legacy_bootstrap(pool).await?;
    }
    ensure_indicator(pool).await?;
    apply_pending(pool).await?;
    Ok(())
}

/// Does the `schema_migrations` indicator table exist yet?
async fn indicator_exists(pool: &Db) -> Result<bool> {
    let name: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    Ok(name.is_some())
}

/// Create the indicator table that records which migrations have been applied.
async fn ensure_indicator(pool: &Db) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("creating schema_migrations table")?;
    Ok(())
}

/// Run every migration whose version is not yet recorded, each in its own
/// transaction, recording it on success.
///
/// Safe to run from two processes at once (e.g. `loom` and a `weaver` CLI
/// invocation both first-opening a fresh database): each migration *claims* its
/// version with `INSERT OR IGNORE` as the transaction's first write, which takes
/// the write lock atomically. The claimer (rows affected = 1) runs the SQL and
/// commits the claim and the change together; a concurrent runner that finds the
/// version already claimed (rows affected = 0) skips it rather than re-applying
/// and colliding on the primary key.
async fn apply_pending(pool: &Db) -> Result<()> {
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations")
        .fetch_all(pool)
        .await
        .context("reading schema_migrations")?;
    for (version, name, sql) in MIGRATIONS {
        // Fast path: a plain read, no lock. The claim below is the real guard.
        if applied.contains(version) {
            continue;
        }
        let mut tx = pool.begin().await?;
        // Claim first, before any read in this transaction, so the INSERT (not a
        // stale-snapshot read) is what acquires the write lock.
        let claimed = sqlx::query(
            "INSERT OR IGNORE INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
        )
        .bind(version)
        .bind(*name)
        .bind(now_iso())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            // Another runner has this version in hand; leave it to them.
            tx.rollback().await?;
            continue;
        }
        tracing::info!(version, name, "applying migration");
        for stmt in split_statements(sql) {
            sqlx::query(&stmt)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("migration {version} ({name}): {stmt}"))?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Split a migration file into individual statements. Strips `--` line comments
/// first so semicolons inside them don't truncate a statement, then splits on
/// `;` and drops empty fragments.
fn split_statements(sql: &str) -> Vec<String> {
    let stripped: String = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<&str>>()
        .join("\n");
    stripped
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// One-time bootstrap for databases that predate the migration framework
// ---------------------------------------------------------------------------

/// Bring a pre-framework (or brand-new) database to the point where the baseline
/// migration applies cleanly. Every step is idempotent and guarded, so this is a
/// no-op on a fresh database and runs at most once on an existing one (after the
/// first run the `schema_migrations` table exists and this is skipped).
async fn legacy_bootstrap(pool: &Db) -> Result<()> {
    drop_legacy_workspaces(pool).await?;
    migrate_issues_to_repo_scope(pool).await?;
    // Additive columns for tables that predate them. No-op when the column is
    // already present, or when the table doesn't exist yet (the baseline
    // migration creates it with the column).
    add_column_if_missing(pool, "branches", "attention", "TEXT NOT NULL DEFAULT 'ok'").await?;
    add_column_if_missing(pool, "issues", "plan_task", "TEXT").await?;
    Ok(())
}

/// Phase 2 clean break: a legacy `workspaces`-shaped database is dropped
/// wholesale before the new schema is applied. Old data is gone by design (the
/// user OK'd this). Detected by the presence of a `workspaces` table; once it's
/// gone the drops never run again.
async fn drop_legacy_workspaces(pool: &Db) -> Result<()> {
    let legacy: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='workspaces'",
    )
    .fetch_optional(pool)
    .await?;
    if legacy.is_none() {
        return Ok(());
    }
    tracing::warn!("dropping legacy workspaces/events/summaries tables for the new schema");
    const DROPS: &[&str] = &[
        "DROP TABLE IF EXISTS workspaces",
        // Old events/summaries pointed at workspace_id; the new tables use
        // branch_id / session_id. Drop them so the baseline CREATE succeeds.
        "DROP TABLE IF EXISTS events",
        "DROP TABLE IF EXISTS summaries",
    ];
    for stmt in DROPS {
        if let Err(e) = sqlx::query(stmt).execute(pool).await {
            tracing::debug!(error = %e, statement = stmt, "drop skipped");
        }
    }
    Ok(())
}

/// One-way migration from the legacy per-branch `issues` table (a `branch_id`
/// FK with `ON DELETE CASCADE`) to the repo-owned shape. Backfills `repo_root`
/// and the `source_branch` / `claimed_branch` annotations from the owning
/// branch — which were 1:1 with the issue, so the post-migration branch view
/// reproduces the old per-branch list exactly. No-op on a fresh or
/// already-migrated database.
async fn migrate_issues_to_repo_scope(pool: &Db) -> Result<()> {
    let cols = table_columns(pool, "issues").await?;
    // Fresh db (table created by the baseline migration) or already migrated.
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

/// Run `ALTER TABLE … ADD COLUMN` when the column is missing. A no-op when the
/// column already exists or the table doesn't exist yet.
async fn add_column_if_missing(pool: &Db, table: &str, column: &str, decl: &str) -> Result<()> {
    let cols = table_columns(pool, table).await?;
    if cols.is_empty() || cols.iter().any(|c| c == column) {
        return Ok(());
    }
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {decl}");
    sqlx::query(&sql)
        .execute(pool)
        .await
        .with_context(|| format!("adding column {table}.{column}"))?;
    tracing::info!(%table, %column, "added column");
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    async fn empty_pool() -> Db {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .journal_mode(SqliteJournalMode::Wal);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap()
    }

    /// Every migration version in order — the expected fully-migrated set, so
    /// these assertions track [`MIGRATIONS`] instead of a hand-kept literal.
    fn all_versions() -> Vec<i64> {
        MIGRATIONS.iter().map(|(v, _, _)| *v).collect()
    }

    /// A fresh database ends up with the current schema, every migration
    /// recorded, and no `notes` table (created by the baseline, dropped by 0002).
    #[tokio::test]
    async fn fresh_database_applies_all_migrations() {
        let pool = empty_pool().await;
        run(&pool).await.unwrap();

        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM schema_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, all_versions());

        assert!(!table_columns(&pool, "branches").await.unwrap().is_empty());
        assert!(
            table_columns(&pool, "notes").await.unwrap().is_empty(),
            "the notes table must be dropped"
        );
    }

    /// Running twice is a no-op: nothing re-applies and the recorded set is
    /// unchanged.
    #[tokio::test]
    async fn migrations_are_idempotent() {
        let pool = empty_pool().await;
        run(&pool).await.unwrap();
        run(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64);
    }

    /// A version already recorded — as if a concurrent runner claimed and applied
    /// it — is skipped (the `INSERT OR IGNORE` claim affects 0 rows, no PK
    /// conflict), while the remaining migrations still apply.
    #[tokio::test]
    async fn already_claimed_versions_are_skipped() {
        let pool = empty_pool().await;
        ensure_indicator(&pool).await.unwrap();
        // Stand up the baseline and record it, mimicking another process having
        // applied migration 1 already.
        for stmt in split_statements(MIGRATIONS[0].2) {
            sqlx::query(&stmt).execute(&pool).await.unwrap();
        }
        sqlx::query(
            "INSERT INTO schema_migrations (version, name, applied_at)
             VALUES (1, 'baseline', '2026-01-01T00:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Must not error on the already-recorded version, and must still run 0002.
        run(&pool).await.unwrap();

        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM schema_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, all_versions());
        assert!(
            table_columns(&pool, "notes").await.unwrap().is_empty(),
            "0002 should still drop the notes table"
        );
    }

    /// An existing database with a populated `notes` table (the pre-framework
    /// world) gets the table dropped on first run, and its data nowhere else.
    #[tokio::test]
    async fn existing_database_drops_notes() {
        let pool = empty_pool().await;
        // Stand up the baseline by hand, as a pre-framework db would have it.
        for stmt in split_statements(MIGRATIONS[0].2) {
            sqlx::query(&stmt).execute(&pool).await.unwrap();
        }
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch) VALUES ('b1', '/repo', 'feature')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO notes (branch_id, text) VALUES ('b1', 'a note')")
            .execute(&pool)
            .await
            .unwrap();

        run(&pool).await.unwrap();

        assert!(
            table_columns(&pool, "notes").await.unwrap().is_empty(),
            "notes must be dropped on an existing database"
        );
        // The branch survived the migration untouched.
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM branches")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 0005 moves the agent's `attention` and an overlooker's `triage` marks
    /// into the `tags` table, drops `ok`/unmarked rows (absence = calm), drops
    /// the five replaced columns, and is idempotent under a re-run.
    #[tokio::test]
    async fn tags_migration_moves_marks_and_drops_columns() {
        let pool = empty_pool().await;
        // Stand up the schema through 0004 only, then seed marks on the old
        // columns as a pre-0005 database would carry them.
        ensure_indicator(&pool).await.unwrap();
        for (version, name, sql) in MIGRATIONS.iter().take_while(|(v, _, _)| *v < 5) {
            for stmt in split_statements(sql) {
                sqlx::query(&stmt).execute(&pool).await.unwrap();
            }
            sqlx::query(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
            )
            .bind(version)
            .bind(*name)
            .bind("2026-01-01T00:00:00.000Z")
            .execute(&pool)
            .await
            .unwrap();
        }
        // A branch the agent flagged AND an overlooker triaged…
        sqlx::query(
            "INSERT INTO branches
               (id, repo_root, branch, attention, triage_level, triage_note, triage_by, triage_at, updated_at)
             VALUES ('b1', '/repo', 'feature', 'blocked', 'attention', 'looks stuck',
                     'status-check', '2026-02-02T00:00:00.000Z', '2026-02-03T00:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // …and a calm branch, whose `ok` marks must NOT become rows.
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch, attention, triage_level)
             VALUES ('b2', '/repo', 'calm', 'ok', '')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run(&pool).await.unwrap();

        // The five columns are gone; `description` and the rest stay.
        let cols = table_columns(&pool, "branches").await.unwrap();
        for gone in [
            "attention",
            "triage_level",
            "triage_note",
            "triage_by",
            "triage_at",
        ] {
            assert!(!cols.iter().any(|c| c == gone), "{gone} must be dropped");
        }
        assert!(cols.iter().any(|c| c == "description"));

        // The non-ok marks moved across with attribution intact.
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT key, value, note, set_by, set_at FROM tags WHERE branch_id = 'b1'
             ORDER BY key",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "attention".into(),
                    "blocked".into(),
                    String::new(),
                    "agent".into(),
                    "2026-02-03T00:00:00.000Z".into(),
                ),
                (
                    "triage".into(),
                    "attention".into(),
                    "looks stuck".into(),
                    "status-check".into(),
                    "2026-02-02T00:00:00.000Z".into(),
                ),
            ]
        );
        // The calm branch contributed no rows.
        let calm: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE branch_id = 'b2'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(calm, 0, "ok/unmarked branches yield no tags");

        // Idempotent: a second run is a no-op and leaves the tags untouched.
        run(&pool).await.unwrap();
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn migrates_legacy_issues_to_repo_scope() {
        // Build a legacy-shaped database by hand: a `branches` table plus the
        // old per-branch `issues` table keyed on `branch_id`.
        let pool = empty_pool().await;
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
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch) VALUES ('b1', '/repo', 'feature')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO issues (branch_id, title, status) VALUES ('b1', 'old issue', 'open')",
        )
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
