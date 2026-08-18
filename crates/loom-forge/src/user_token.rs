//! A user's own GitHub token (a fine-grained PAT), stored in the
//! `user_github_tokens` table.
//!
//! Loom selects this token for an ordinary interactive session launched by that
//! user. Sessions use their profile-approved GitHub App credential when the
//! Account PAT is not selected. Restricted sessions use Loom's App-backed fixed
//! GitHub tools.
//!
//! The value is **write-only** over the API: callers learn only *that* a token is
//! set and when it changed, never the token itself. Export into ordinary shared
//! sessions is blast-radius reduction rather than isolation; restricted
//! sessions avoid that export entirely.

use anyhow::Result;
use serde::Serialize;
use sqlx::Row;

use crate::db::{now_iso, Db};

/// Whether a user has a token set, and when it last changed — the write-only
/// status the account pane renders and the API returns. Never the token.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenStatus {
    pub set: bool,
    pub updated_at: Option<String>,
}

/// The stored token for `username`, if any. Used only while assembling the
/// user's ordinary interactive session environment and never exposed by API.
pub async fn get(db: &Db, username: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT token FROM user_github_tokens WHERE username = ?")
        .bind(username)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.get::<String, _>("token")))
}

/// Whether `username` has a token set, plus its timestamp — the write-only view.
pub async fn status(db: &Db, username: &str) -> Result<TokenStatus> {
    let row = sqlx::query("SELECT updated_at FROM user_github_tokens WHERE username = ?")
        .bind(username)
        .fetch_optional(db)
        .await?;
    Ok(match row {
        Some(r) => TokenStatus {
            set: true,
            updated_at: Some(r.get::<String, _>("updated_at")),
        },
        None => TokenStatus {
            set: false,
            updated_at: None,
        },
    })
}

/// Upsert `username`'s token.
pub async fn set(db: &Db, username: &str, token: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO user_github_tokens (username, token, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(username) DO UPDATE
           SET token = excluded.token, updated_at = excluded.updated_at",
    )
    .bind(username)
    .bind(token)
    .bind(&now)
    .execute(db)
    .await?;
    tracing::info!(username, "github token set");
    Ok(())
}

/// Delete `username`'s token. Removing an absent token is a no-op (`false`).
pub async fn remove(db: &Db, username: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM user_github_tokens WHERE username = ?")
        .bind(username)
        .execute(db)
        .await?;
    let removed = res.rows_affected() > 0;
    tracing::info!(username, removed, "github token removed");
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_user(db: &Db, username: &str) {
        sqlx::query("INSERT INTO users (username) VALUES (?)")
            .bind(username)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_get_status_remove_round_trip() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;

        assert_eq!(
            status(&db, "alice").await.unwrap(),
            TokenStatus {
                set: false,
                updated_at: None
            }
        );
        assert!(get(&db, "alice").await.unwrap().is_none());

        set(&db, "alice", "github_pat_abc").await.unwrap();
        assert_eq!(
            get(&db, "alice").await.unwrap().as_deref(),
            Some("github_pat_abc")
        );
        let status = status(&db, "alice").await.unwrap();
        assert!(status.set);
        assert!(status.updated_at.is_some());

        set(&db, "alice", "github_pat_rotated").await.unwrap();
        assert_eq!(
            get(&db, "alice").await.unwrap().as_deref(),
            Some("github_pat_rotated")
        );
        assert!(remove(&db, "alice").await.unwrap());
        assert!(!remove(&db, "alice").await.unwrap());
        assert!(get(&db, "alice").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn tokens_are_scoped_per_user() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_user(&db, "alice").await;
        seed_user(&db, "bob").await;
        set(&db, "alice", "alice-tok").await.unwrap();
        set(&db, "bob", "bob-tok").await.unwrap();

        assert_eq!(
            get(&db, "alice").await.unwrap().as_deref(),
            Some("alice-tok")
        );
        assert_eq!(get(&db, "bob").await.unwrap().as_deref(), Some("bob-tok"));
        remove(&db, "alice").await.unwrap();
        assert!(get(&db, "alice").await.unwrap().is_none());
        assert_eq!(get(&db, "bob").await.unwrap().as_deref(), Some("bob-tok"));
    }
}
