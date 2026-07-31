//! Fully initialized Loom database composition.

use anyhow::Result;
use std::path::Path;

pub use loom_store::db::{
    core_connect_in_memory, default_db_path, latest_migration_version, now_iso, run_dir,
    weaver_home, Db,
};

async fn initialize(db: &Db) -> Result<()> {
    crate::profile::backfill_mcp_policies(db).await?;
    crate::profile::normalize_default(db).await?;
    crate::profile::seed_stock_profiles(db).await?;
    crate::runs::reconcile_missing_sessions(db).await?;
    Ok(())
}

pub async fn connect(path: &Path) -> Result<Db> {
    let db = loom_store::db::connect(path).await?;
    initialize(&db).await?;
    Ok(db)
}

pub async fn connect_in_memory() -> Result<Db> {
    let db = loom_store::db::connect_in_memory().await?;
    initialize(&db).await?;
    Ok(db)
}
