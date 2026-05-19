//! Key/value settings stored in the `settings` table.

use anyhow::Result;
use sqlx::Row;

use crate::db::Db;

pub const DEFAULT_AGENT: &str = "claude";
pub const DEFAULT_SUMMARY_INTERVAL_SECS: i64 = 600;

pub async fn get(db: &Db, key: &str) -> Option<String> {
    sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<String, _>("value"))
}

pub async fn get_or(db: &Db, key: &str, default: &str) -> String {
    get(db, key).await.unwrap_or_else(|| default.to_string())
}

pub async fn get_i64(db: &Db, key: &str, default: i64) -> i64 {
    get(db, key)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub async fn set(db: &Db, key: &str, value: &str) -> Result<()> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn list(db: &Db) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query("SELECT key, value FROM settings ORDER BY key")
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("key"), r.get::<String, _>("value")))
        .collect())
}
