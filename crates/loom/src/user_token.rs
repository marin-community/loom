//! A user's own GitHub token (a fine-grained PAT), stored in the
//! `user_github_tokens` table.
//!
//! When that user launches an interactive session, loom injects the token as
//! `GH_TOKEN` into the session env ([`crate::web::sessions::create_session_core`]),
//! overriding the shared ambient `GH_TOKEN` from the deploy env — so the agent's
//! `git push` / `gh` act as *that user*. Combined with the per-user commit author
//! identity loom already sets ([`crate::auth::commit_identity`]), both the commit
//! and the push are attributed to them.
//!
//! The value is **write-only** over the API: callers learn only *that* a token is
//! set and when it changed, never the token itself — the same treatment
//! [`crate::repo_env`] gives per-repo secrets. This is blast-radius reduction, not
//! isolation: in loom's single shared container any agent can still read the
//! exported `GH_TOKEN` (shared-loom design §6.4).

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

/// The stored token for `username`, if any. The *only* reader of the value; used
/// at session launch and never exposed over the API.
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
    tracing::debug!(username, "user github token set");
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

        // Absent: status reports unset, get returns None.
        assert_eq!(
            status(&db, "alice").await.unwrap(),
            TokenStatus {
                set: false,
                updated_at: None
            }
        );
        assert!(get(&db, "alice").await.unwrap().is_none());

        // Set: get returns the value; status reports set with a timestamp, but
        // the value is only ever readable through `get` (never `status`).
        set(&db, "alice", "github_pat_abc").await.unwrap();
        assert_eq!(
            get(&db, "alice").await.unwrap().as_deref(),
            Some("github_pat_abc")
        );
        let st = status(&db, "alice").await.unwrap();
        assert!(st.set);
        assert!(st.updated_at.is_some());

        // Upsert replaces in place.
        set(&db, "alice", "github_pat_rotated").await.unwrap();
        assert_eq!(
            get(&db, "alice").await.unwrap().as_deref(),
            Some("github_pat_rotated")
        );

        // Remove, then removing again is a no-op.
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

        // Removing one leaves the other untouched.
        remove(&db, "alice").await.unwrap();
        assert!(get(&db, "alice").await.unwrap().is_none());
        assert_eq!(get(&db, "bob").await.unwrap().as_deref(), Some("bob-tok"));
    }
}
