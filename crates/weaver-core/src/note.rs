//! Progress notes appended by the agent (`weaver note ...`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{now_iso, Db};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Note {
    pub id: i64,
    pub branch_id: String,
    pub text: String,
    pub created_at: String,
}

pub async fn add(db: &Db, branch_id: &str, text: &str) -> Result<Note> {
    let now = now_iso();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO notes (branch_id, text, created_at) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(branch_id)
    .bind(text)
    .bind(&now)
    .fetch_one(db)
    .await?;
    Ok(Note {
        id: row.0,
        branch_id: branch_id.to_string(),
        text: text.to_string(),
        created_at: now,
    })
}

pub async fn list_for_branch(db: &Db, branch_id: &str) -> Result<Vec<Note>> {
    let rows = sqlx::query_as::<_, Note>("SELECT * FROM notes WHERE branch_id = ? ORDER BY id ASC")
        .bind(branch_id)
        .fetch_all(db)
        .await?;
    Ok(rows)
}
