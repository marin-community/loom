//! Per-repo environment variables, stored in the `repo_env` table.
//!
//! These layer on top of the operator's global [`crate::agent_env`] when a
//! session launches against a repo: the resolved env is
//! `agent_env` < `repo_env` < the repo's own `.weaver/config.toml` `[env]`
//! ([`weaver_core::repo_config`]), so a per-repo value overrides a global one and
//! the committed repo file overrides both. They are exported into the interactive
//! agent terminal alongside loom's `WEAVER_*` / `LOOM_TOKEN`.
//!
//! Values are **write-only**: the API returns names and timestamps but never the
//! value, because these hold per-repo secrets (a registry token, a database URL)
//! — the same treatment the OAuth secret gets. This is blast-radius reduction,
//! not isolation: in loom's single shared container any agent can still read the
//! exported environment. See the shared-loom design §6.4.
//!
//! Names are validated as POSIX shell identifiers and may not use loom's reserved
//! `WEAVER_`/`LOOM_` prefixes — the same rule as `agent_env`, reused via
//! [`crate::agent_env::validate_name`], since `repo_env` is exported by the same
//! launch script.

use anyhow::Result;
use serde::Serialize;
use sqlx::Row;

use crate::db::{now_iso, Db};

/// One stored variable's *metadata* — what the API returns and the settings pane
/// renders. The value is deliberately omitted (write-only); only that a value is
/// set, under what name, and when it last changed.
#[derive(Debug, Clone, Serialize)]
pub struct RepoEnvVar {
    pub name: String,
    pub updated_at: String,
}

/// The variables' metadata for a repo, ordered by name. Never includes values.
pub async fn list(db: &Db, repo_root: &str) -> Result<Vec<RepoEnvVar>> {
    let rows =
        sqlx::query("SELECT name, updated_at FROM repo_env WHERE repo_root = ? ORDER BY name")
            .bind(repo_root)
            .fetch_all(db)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| RepoEnvVar {
            name: r.get::<String, _>("name"),
            updated_at: r.get::<String, _>("updated_at"),
        })
        .collect())
}

/// The repo's variables as `(name, value)` pairs — what the launch env layering
/// consumes. Ordered by name, like [`list`]. This is the only path that reads the
/// stored values; it is never exposed over the API.
pub async fn pairs(db: &Db, repo_root: &str) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query("SELECT name, value FROM repo_env WHERE repo_root = ? ORDER BY name")
        .bind(repo_root)
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("name"), r.get::<String, _>("value")))
        .collect())
}

/// Upsert one variable for a repo. The caller is expected to
/// [`crate::agent_env::validate_name`] first; this only touches the database.
pub async fn set(db: &Db, repo_root: &str, name: &str, value: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO repo_env (repo_root, name, value, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(repo_root, name) DO UPDATE
           SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(repo_root)
    .bind(name)
    .bind(value)
    .bind(&now)
    .execute(db)
    .await?;
    tracing::debug!(repo_root, name, "repo_env set");
    Ok(())
}

/// Delete one variable. Removing an absent name is a no-op (returns `false`).
pub async fn remove(db: &Db, repo_root: &str, name: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM repo_env WHERE repo_root = ? AND name = ?")
        .bind(repo_root)
        .bind(name)
        .execute(db)
        .await?;
    let removed = res.rows_affected() > 0;
    tracing::debug!(repo_root, name, removed, "repo_env remove");
    Ok(removed)
}

/// Overlay `over` onto `base` in place: a name already present is overwritten
/// (the higher layer wins) keeping its position; a new name is appended. The
/// launch env is built by layering each source in priority order — global
/// `agent_env`, then `repo_env`, then the repo file's `[env]` — so the last
/// writer of any name wins, while preserving a stable export order.
pub fn layer(base: &mut Vec<(String, String)>, over: impl IntoIterator<Item = (String, String)>) {
    for (key, value) in over {
        match base.iter_mut().find(|(name, _)| *name == key) {
            Some(slot) => slot.1 = value,
            None => base.push((key, value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_list_pairs_remove_are_repo_scoped() {
        let db = crate::db::connect_in_memory().await.unwrap();
        assert!(list(&db, "/repo/a").await.unwrap().is_empty());

        set(&db, "/repo/a", "TOKEN", "secret").await.unwrap();
        set(&db, "/repo/a", "REGION", "us").await.unwrap();
        set(&db, "/repo/b", "TOKEN", "other").await.unwrap();

        // list() is per-repo, ordered by name, and never leaks the value.
        let a = list(&db, "/repo/a").await.unwrap();
        let names: Vec<&str> = a.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["REGION", "TOKEN"]);
        let json = serde_json::to_value(&a[0]).unwrap();
        assert!(
            json.get("value").is_none(),
            "value must never be serialized"
        );

        // pairs() (launch-only) does carry the values.
        let pairs_a = pairs(&db, "/repo/a").await.unwrap();
        assert_eq!(
            pairs_a,
            vec![
                ("REGION".to_string(), "us".to_string()),
                ("TOKEN".to_string(), "secret".to_string()),
            ]
        );
        // Scoped: repo b only sees its own.
        assert_eq!(
            pairs(&db, "/repo/b").await.unwrap(),
            vec![("TOKEN".to_string(), "other".to_string())]
        );

        // Upsert replaces in place.
        set(&db, "/repo/a", "TOKEN", "rotated").await.unwrap();
        assert_eq!(
            pairs(&db, "/repo/a").await.unwrap()[1],
            ("TOKEN".to_string(), "rotated".to_string())
        );

        assert!(remove(&db, "/repo/a", "TOKEN").await.unwrap());
        assert!(!remove(&db, "/repo/a", "TOKEN").await.unwrap());
        assert_eq!(list(&db, "/repo/a").await.unwrap().len(), 1);
        // Removing from a did not touch b.
        assert_eq!(pairs(&db, "/repo/b").await.unwrap().len(), 1);
    }

    #[test]
    fn layer_overrides_in_place_and_appends_new() {
        let mut env = vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ];
        // B overridden in place; C appended.
        layer(
            &mut env,
            vec![
                ("B".to_string(), "20".to_string()),
                ("C".to_string(), "3".to_string()),
            ],
        );
        assert_eq!(
            env,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "20".to_string()),
                ("C".to_string(), "3".to_string()),
            ]
        );
    }
}
