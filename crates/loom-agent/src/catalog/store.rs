//! The harness model catalogue. One row per `(harness, model)`.
//!
//! `options_json` stays null until a fetch for that model succeeds. A null
//! row is still part of the model list, so the picker can show the model
//! while the next incremental pass retries its options.

use anyhow::Result;
use serde_json::Value;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone)]
pub struct CatalogRow {
    pub model_id: String,
    pub label: String,
    pub options_json: Option<String>,
    pub adapter_version: String,
    pub credential_fingerprint: String,
    pub fetched_at: Option<String>,
}

impl CatalogRow {
    pub fn options(&self) -> Option<Vec<Value>> {
        let raw = self.options_json.as_deref()?;
        serde_json::from_str(raw).ok()
    }
}

pub async fn list(db: &Db, harness: &str) -> Result<Vec<CatalogRow>> {
    let rows = sqlx::query_as::<_, Row>(
        "SELECT model_id, label, options_json, adapter_version, credential_fingerprint, fetched_at
         FROM harness_model_catalog WHERE harness = ? ORDER BY model_id",
    )
    .bind(harness)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn has_rows(db: &Db, harness: &str) -> Result<bool> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM harness_model_catalog WHERE harness = ?")
            .bind(harness)
            .fetch_one(db)
            .await?;
    Ok(count > 0)
}

/// Record a model from the adapter's list without touching a fetched option set.
pub async fn note_listed(db: &Db, harness: &str, model_id: &str, label: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO harness_model_catalog (harness, model_id, label)
         VALUES (?, ?, ?)
         ON CONFLICT(harness, model_id) DO UPDATE SET label = excluded.label",
    )
    .bind(harness)
    .bind(model_id)
    .bind(label)
    .execute(db)
    .await?;
    Ok(())
}

/// Persist one model's raw option set as soon as its fetch lands.
pub async fn write_options(
    db: &Db,
    harness: &str,
    model_id: &str,
    label: &str,
    options: &[Value],
    adapter_version: &str,
) -> Result<()> {
    let options_json = serde_json::to_string(options)?;
    sqlx::query(
        "INSERT INTO harness_model_catalog
            (harness, model_id, label, options_json, adapter_version, fetched_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(harness, model_id) DO UPDATE SET
            label = excluded.label,
            options_json = excluded.options_json,
            adapter_version = excluded.adapter_version,
            fetched_at = excluded.fetched_at",
    )
    .bind(harness)
    .bind(model_id)
    .bind(label)
    .bind(options_json)
    .bind(adapter_version)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn prune(db: &Db, harness: &str, keep: &[String]) -> Result<()> {
    let rows = list(db, harness).await?;
    for row in rows {
        if !keep.iter().any(|id| id == &row.model_id) {
            sqlx::query("DELETE FROM harness_model_catalog WHERE harness = ? AND model_id = ?")
                .bind(harness)
                .bind(&row.model_id)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

pub async fn stamp_fingerprint(db: &Db, harness: &str, fingerprint: &str) -> Result<()> {
    sqlx::query("UPDATE harness_model_catalog SET credential_fingerprint = ? WHERE harness = ?")
        .bind(fingerprint)
        .bind(harness)
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    model_id: String,
    label: String,
    options_json: Option<String>,
    adapter_version: String,
    credential_fingerprint: String,
    fetched_at: Option<String>,
}

impl From<Row> for CatalogRow {
    fn from(row: Row) -> Self {
        Self {
            model_id: row.model_id,
            label: row.label,
            options_json: row.options_json,
            adapter_version: row.adapter_version,
            credential_fingerprint: row.credential_fingerprint,
            fetched_at: row.fetched_at,
        }
    }
}
