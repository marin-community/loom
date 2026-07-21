use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::sync::{Mutex, MutexGuard};

pub type Db = SqlitePool;

// SQLite permits one writer at a time. Coordinate multi-statement writers
// before they check out a pooled connection: relying on SQLite's busy handler
// lets every contender occupy a connection while it waits, which can exhaust
// the pool and make unrelated dashboard reads look hung.
static WRITE_LOCK: Mutex<()> = Mutex::const_new(());

/// A SQLite write transaction holding the process-wide writer permit.
///
/// The permit is released after the transaction is committed, rolled back, or
/// dropped. Dereferencing exposes the connection so existing sqlx query calls
/// can execute with `&mut *tx`.
pub struct ImmediateTransaction<'a> {
    tx: Transaction<'a, Sqlite>,
    _writer: MutexGuard<'static, ()>,
}

impl Deref for ImmediateTransaction<'_> {
    type Target = SqliteConnection;

    fn deref(&self) -> &Self::Target {
        &self.tx
    }
}

impl DerefMut for ImmediateTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tx
    }
}

impl ImmediateTransaction<'_> {
    pub async fn commit(self) -> sqlx::Result<()> {
        self.tx.commit().await
    }

    pub async fn rollback(self) -> sqlx::Result<()> {
        self.tx.rollback().await
    }
}

/// A write transaction on [`Db`]. Callers hold this alias rather than naming
/// the backend's transaction type themselves.
pub type DbTransaction<'a> = ImmediateTransaction<'a>;
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

/// The `host:port` of a running loom dashboard, for building shareable URLs
/// (e.g. the `weaver artifact write` write-back link). Resolved the way loom's
/// own clients resolve it, minus a dependency on the loom crate:
///
///   1. `$WEAVER_API` (a URL or bare `host:port`, normalized to `host:port`),
///   2. the address loom recorded in `<weaver_home>/loom.json` while serving.
///
/// `None` when neither is set — typically no loom is running. Callers fall back
/// to a daemon-less message; an artifact write is a plain DB write either way.
pub fn dashboard_addr() -> Option<String> {
    if let Ok(api) = std::env::var("WEAVER_API") {
        let socket = api
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        if !socket.is_empty() {
            return Some(socket);
        }
    }
    // loom writes `{ "pid", "addr", "started_at" }` to `loom.json` under the
    // weaver home while it serves (see loom::server::ServerState); read the
    // `addr` field without taking on the loom dependency.
    let text = std::fs::read_to_string(weaver_home().join("loom.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("addr")
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Current UTC time as an ISO-8601 string, matching the SQLite default format.
pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// The instant `days` days from now, in the same stored format as [`now_iso`].
/// Expiries are computed app-side and bound as parameters so queries carry no
/// backend-specific date arithmetic; the format orders lexicographically, so
/// string comparison against `*_at` columns is sound. Returns `None` when the
/// requested interval or resulting instant is outside chrono's range.
pub fn iso_in_days(days: i64) -> Option<String> {
    let delta = chrono::TimeDelta::try_days(days)?;
    chrono::Utc::now()
        .checked_add_signed(delta)
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// Open (creating if missing) and migrate the on-disk database.
///
/// Backend-specific surface, kept here and in [`begin_immediate`]: the
/// `sqlite:` DSN string, the WAL journal mode, the `busy_timeout`, and the
/// migrate-then-reopen connection dance below.
pub async fn connect(path: &Path) -> Result<Db> {
    tracing::info!(path = %path.display(), "opening database");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db directory {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
        .with_context(|| format!("invalid database path {}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        // A writer that loses the lock race waits its turn instead of failing
        // with SQLITE_BUSY "database is locked". Only effective for writes that
        // take the lock up front — see [`begin_immediate`].
        .busy_timeout(std::time::Duration::from_secs(5));

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
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Open a write transaction that takes SQLite's write lock up front
/// (`BEGIN IMMEDIATE`). A default deferred transaction starts as a reader and
/// upgrades on its first write — and an upgrade that loses the race fails with
/// SQLITE_BUSY *immediately*, bypassing the connection's `busy_timeout`.
///
/// Contenders queue on [`WRITE_LOCK`] before acquiring a pool connection. This
/// preserves read capacity while writes wait for SQLite's single writer slot.
///
/// Backend-specific surface (with [`connect`]): the lock-upgrade behaviour and
/// the `BEGIN IMMEDIATE` statement are SQLite's.
pub async fn begin_immediate(db: &Db) -> sqlx::Result<DbTransaction<'static>> {
    let writer = WRITE_LOCK.lock().await;
    let tx = db.begin_with("BEGIN IMMEDIATE").await?;
    Ok(ImmediateTransaction {
        tx,
        _writer: writer,
    })
}

/// Apply pending schema migrations. The migration framework (ordered SQL files
/// + a `schema_migrations` indicator table) lives in [`crate::migrations`].
async fn migrate(pool: &Db) -> Result<()> {
    crate::migrations::run(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::task::Poll;

    #[test]
    fn iso_in_days_uses_the_shared_sortable_timestamp_format() {
        let before = chrono::Utc::now();
        let encoded = iso_in_days(2).unwrap();
        let after = chrono::Utc::now();
        let parsed = chrono::DateTime::parse_from_rfc3339(&encoded)
            .unwrap()
            .with_timezone(&chrono::Utc);

        // Formatting truncates sub-millisecond precision, hence the small
        // tolerance around the time spent inside `iso_in_days`.
        assert!(parsed >= before + chrono::Duration::days(2) - chrono::Duration::milliseconds(2));
        assert!(parsed <= after + chrono::Duration::days(2) + chrono::Duration::milliseconds(2));
        assert_eq!(encoded.len(), "2026-01-01T00:00:00.000Z".len());
        assert!(encoded.ends_with('Z'));
        assert!(iso_in_days(i64::MAX).is_none());
    }

    #[tokio::test]
    async fn immediate_transactions_use_the_facade_alias() {
        let db = connect_in_memory().await.unwrap();
        let tx: DbTransaction<'_> = begin_immediate(&db).await.unwrap();
        tx.rollback().await.unwrap();
    }

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

    /// Waiting for SQLite's single writer must not consume every pooled
    /// connection. Dashboard reads should remain available while writes queue.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiting_writers_do_not_starve_reads() {
        let dir = tempfile::tempdir().unwrap();
        let db = connect(&dir.path().join("weaver.db")).await.unwrap();

        // Pre-open the whole pool so polling each waiter once gets as far as
        // the writer lock rather than pausing to establish a connection.
        let mut connections = Vec::new();
        for _ in 0..5 {
            connections.push(db.acquire().await.unwrap());
        }
        drop(connections);

        let writer = begin_immediate(&db).await.unwrap();

        let mut waiters = (0..4)
            .map(|_| Box::pin(begin_immediate(&db)))
            .collect::<Vec<_>>();
        for waiter in &mut waiters {
            std::future::poll_fn(|cx| {
                assert!(
                    waiter.as_mut().poll(cx).is_pending(),
                    "waiter unexpectedly acquired SQLite's writer lock"
                );
                Poll::Ready(())
            })
            .await;
        }

        let read = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&db),
        )
        .await;

        writer.commit().await.unwrap();
        for waiter in waiters {
            waiter.await.unwrap().commit().await.unwrap();
        }
        assert!(
            read.is_ok(),
            "writer contention exhausted the pool and blocked an unrelated read"
        );
    }
}
