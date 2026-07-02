//! Operator-managed environment variables, stored in the `agent_env` table.
//!
//! These are exported into every interactive agent session loom launches —
//! alongside loom's own `WEAVER_*` / `LOOM_TOKEN` — so the operator can add a
//! registry token, a `GH_HOST`, an `ANTHROPIC_BASE_URL`, etc. at runtime from
//! the settings pane, without rebuilding the image or editing the deploy env
//! file. They are NOT applied to the env-stripped one-shot judgement agent
//! (see [`crate::agent`]).
//!
//! This is a flat name/value store: unlike [`crate::config`] there is no
//! registry of known keys, because the whole point is arbitrary,
//! deploy-specific variables. The only constraint is that a name is a valid
//! POSIX shell identifier, since the value is exported by the launch script.

use anyhow::Result;
use serde::Serialize;
use sqlx::Row;

use crate::db::{now_iso, Db};

/// One stored variable with its bookkeeping timestamp — what the settings pane
/// renders and what the API returns.
#[derive(Debug, Clone, Serialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    pub updated_at: String,
}

/// Names loom owns and exports itself ([`crate::agent::launch`]). Operator vars
/// are exported *after* these, so allowing one of these names through would let a
/// stored value shadow `WEAVER_API`/`WEAVER_BRANCH`/`LOOM_TOKEN` and break the
/// agent's own `loom session …` calls. We reserve the whole `WEAVER_`/`LOOM_`
/// prefix space rather than just the current names so future loom-owned vars are
/// covered without a matching validation change.
const RESERVED_PREFIXES: &[&str] = &["WEAVER_", "LOOM_"];

/// Validate an environment-variable name. Accept the POSIX-portable identifier
/// shape (`[A-Za-z_][A-Za-z0-9_]*`): a leading letter or underscore, then
/// letters, digits, or underscores. This is exactly what the `export NAME=…` in
/// the launch script can carry, and rejecting anything else keeps a stray name
/// from corrupting the script. Names in loom's own [`RESERVED_PREFIXES`] are also
/// rejected so an operator var can't shadow the environment loom needs. The error
/// is a key-free reason so callers can prefix it with whatever context they like.
pub fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("name must not be empty".to_string());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "name must start with a letter or underscore, got '{name}'"
        ));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "name may contain only letters, digits, and underscores, got '{name}'"
        ));
    }
    if let Some(prefix) = RESERVED_PREFIXES.iter().find(|p| name.starts_with(**p)) {
        return Err(format!(
            "name '{name}' is reserved: the '{prefix}' prefix is used by loom's own \
             environment and cannot be overridden"
        ));
    }
    Ok(())
}

/// Every stored variable, ordered by name.
pub async fn list(db: &Db) -> Result<Vec<EnvVar>> {
    let rows = sqlx::query("SELECT name, value, updated_at FROM agent_env ORDER BY name")
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| EnvVar {
            name: r.get::<String, _>("name"),
            value: r.get::<String, _>("value"),
            updated_at: r.get::<String, _>("updated_at"),
        })
        .collect())
}

/// The variables as a plain `(name, value)` list — what [`crate::agent::launch`]
/// exports into a session. Same order as [`list`].
pub async fn pairs(db: &Db) -> Result<Vec<(String, String)>> {
    Ok(list(db)
        .await?
        .into_iter()
        .map(|e| (e.name, e.value))
        .collect())
}

/// Upsert one variable. The caller is expected to [`validate_name`] first; this
/// only touches the database.
pub async fn set(db: &Db, name: &str, value: &str) -> Result<()> {
    let now = now_iso();
    sqlx::query(
        "INSERT INTO agent_env (name, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(name) DO UPDATE
           SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(name)
    .bind(value)
    .bind(&now)
    .execute(db)
    .await?;
    tracing::debug!(name, "agent_env set");
    tracing::info!(name, "env var set");
    Ok(())
}

/// Delete one variable. Removing an absent name is a no-op (returns `false`).
pub async fn remove(db: &Db, name: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM agent_env WHERE name = ?")
        .bind(name)
        .execute(db)
        .await?;
    let removed = res.rows_affected() > 0;
    tracing::debug!(name, removed, "agent_env remove");
    tracing::info!(name, removed, "env var deleted");
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_shell_identifiers() {
        assert!(validate_name("FOO").is_ok());
        assert!(validate_name("_x").is_ok());
        assert!(validate_name("GH_HOST").is_ok());
        assert!(validate_name("ANTHROPIC_BASE_URL2").is_ok());
    }

    #[test]
    fn validate_name_rejects_non_identifiers() {
        assert!(validate_name("").is_err());
        assert!(validate_name("1FOO").is_err());
        assert!(validate_name("FOO-BAR").is_err());
        assert!(validate_name("FOO BAR").is_err());
        assert!(validate_name("FOO=BAR").is_err());
        assert!(validate_name("café").is_err());
    }

    #[test]
    fn validate_name_rejects_loom_reserved_names() {
        // Operator vars are exported after loom's own, so these must not be
        // settable or they'd shadow the environment the agent depends on.
        assert!(validate_name("WEAVER_API").is_err());
        assert!(validate_name("WEAVER_BRANCH").is_err());
        assert!(validate_name("LOOM_TOKEN").is_err());
        assert!(validate_name("WEAVER_ANYTHING").is_err());
    }

    #[tokio::test]
    async fn set_get_remove_round_trip() {
        let db = crate::db::connect_in_memory().await.unwrap();
        assert!(list(&db).await.unwrap().is_empty());

        set(&db, "GH_HOST", "github.example.com").await.unwrap();
        set(&db, "API_TOKEN", "secret").await.unwrap();
        let all = list(&db).await.unwrap();
        // Ordered by name: API_TOKEN before GH_HOST.
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "API_TOKEN");
        assert_eq!(all[1].name, "GH_HOST");
        assert_eq!(all[1].value, "github.example.com");

        // Upsert replaces the value in place.
        set(&db, "GH_HOST", "github.internal").await.unwrap();
        let pairs = pairs(&db).await.unwrap();
        assert_eq!(
            pairs,
            vec![
                ("API_TOKEN".to_string(), "secret".to_string()),
                ("GH_HOST".to_string(), "github.internal".to_string()),
            ]
        );

        assert!(remove(&db, "API_TOKEN").await.unwrap());
        // Removing again is a no-op.
        assert!(!remove(&db, "API_TOKEN").await.unwrap());
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }
}
