use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub type Db = SqlitePool;

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
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
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

    // Apply migrations on a dedicated single connection, then close it, *before*
    // the shared read/write pool opens. A column-adding migration (`ALTER TABLE
    // … ADD COLUMN`) changes a table's column count mid-run; if migrations ran on
    // the multi-connection pool, a connection that cached the table at its old
    // width could later be reused for a `SELECT *` and decode a short row against
    // the wider struct — an out-of-bounds panic. Finalising the schema on one
    // connection first means every pooled connection sees the finished schema on
    // its first use.
    {
        let migrator = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .with_context(|| format!("opening database {} to migrate", path.display()))?;
        migrate(&migrator).await?;
        migrator.close().await;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;
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

/// Apply pending schema migrations. The migration framework (ordered SQL files
/// + a `schema_migrations` indicator table) lives in [`crate::migrations`].
async fn migrate(pool: &Db) -> Result<()> {
    crate::migrations::run(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the on-disk pool must serve a fully-migrated schema on every
    /// connection. A schema-changing migration once left some pooled connections
    /// with a stale, narrower view of `branches`, so a later `SELECT *`
    /// (`branch::get`) decoded a short row against the wider struct and panicked.
    /// Migrating on a dedicated connection before the pool opens fixes it; this
    /// exercises the file path (not the single-connection in-memory pool). The
    /// 0005 migration *drops* columns, so a stale-schema connection would decode
    /// a wider row against the narrower struct — the same failure mode in reverse.
    #[tokio::test]
    async fn on_disk_pool_reads_the_full_migrated_branch() {
        let dir = tempfile::tempdir().unwrap();
        let db = connect(&dir.path().join("weaver.db")).await.unwrap();
        let b = crate::branch::upsert(&db, "/r", "main", "main")
            .await
            .unwrap();
        // The branch decodes cleanly against the post-migration schema.
        let got = crate::branch::get(&db, &b.id).await.unwrap().unwrap();
        assert_eq!(got.branch, "main");
        // And the tags table (created by 0005) is usable on the shared pool.
        assert!(crate::tags::get(&db, &b.id, crate::tags::ATTENTION_KEY)
            .await
            .unwrap()
            .is_none());
        crate::tags::set(
            &db,
            &b.id,
            crate::tags::ATTENTION_KEY,
            "blocked",
            "",
            "agent",
        )
        .await
        .unwrap();
        let tag = crate::tags::get(&db, &b.id, crate::tags::ATTENTION_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tag.value, "blocked");
    }

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
}
