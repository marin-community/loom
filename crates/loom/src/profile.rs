//! Named, reusable session launch posture and environment.
//!
//! `default` is the compatibility boundary for the former flat `agent.*`
//! settings and `agent_env` table. New launches resolve one profile and stamp
//! its non-secret policy onto the session; profile environment values remain
//! rotatable and are loaded again on a real respawn.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::{now_iso, Db};

pub const DEFAULT_PROFILE: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Profile {
    pub name: String,
    pub description: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub protocol: String,
    pub mode: String,
    pub class: String,
    pub strict: bool,
    pub env_clear: bool,
    /// JSON array in storage; parsed through [`ambient_names`].
    pub ambient_allowlist: String,
    pub idle_archive_secs: Option<i64>,
    pub max_concurrent: i64,
    pub turn_budget: Option<i64>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl Profile {
    pub fn ambient_names(&self) -> Result<Vec<String>> {
        serde_json::from_str(&self.ambient_allowlist).context("invalid profile ambient allowlist")
    }

    pub fn is_automation_safe(&self) -> bool {
        self.strict && self.env_clear && self.class == "automation"
    }

    pub fn as_input(&self) -> Result<ProfileInput> {
        Ok(ProfileInput {
            name: self.name.clone(),
            description: self.description.clone(),
            agent_kind: self.agent_kind.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            protocol: self.protocol.clone(),
            mode: self.mode.clone(),
            class: self.class.clone(),
            strict: self.strict,
            env_clear: self.env_clear,
            ambient_allowlist: self.ambient_names()?,
            idle_archive_secs: self.idle_archive_secs,
            max_concurrent: self.max_concurrent,
            turn_budget: self.turn_budget,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub agent_kind: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default = "default_class")]
    pub class: String,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub env_clear: bool,
    #[serde(default)]
    pub ambient_allowlist: Vec<String>,
    #[serde(default)]
    pub idle_archive_secs: Option<i64>,
    #[serde(default)]
    pub max_concurrent: i64,
    #[serde(default)]
    pub turn_budget: Option<i64>,
}

fn default_class() -> String {
    "interactive".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProfileEnvMeta {
    pub name: String,
    pub updated_at: String,
}

pub fn validate_name(name: &str) -> std::result::Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("profile name must not be empty".to_string());
    };
    if !first.is_ascii_alphabetic() {
        return Err("profile name must start with an ASCII letter".to_string());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        return Err("profile name may contain only letters, digits, '-' and '_'".to_string());
    }
    if name.len() > 64 {
        return Err("profile name must be at most 64 bytes".to_string());
    }
    Ok(())
}

async fn validate_input(db: &Db, input: &ProfileInput) -> Result<(String, String)> {
    validate_name(input.name.trim()).map_err(|e| anyhow!(e))?;
    if !matches!(input.class.trim(), "interactive" | "automation") {
        bail!("profile class must be 'interactive' or 'automation'");
    }
    if input.class.trim() == "automation" && input.strict && !input.env_clear {
        bail!("strict automation profiles must clear the ambient environment");
    }
    for name in &input.ambient_allowlist {
        crate::agent_env::validate_name(name).map_err(|e| anyhow!(e))?;
    }
    if input.idle_archive_secs.is_some_and(|v| v < 0)
        || input.turn_budget.is_some_and(|v| v < 0)
        || input.max_concurrent < 0
    {
        bail!("profile limits must be zero or positive");
    }
    let agent_kind = input.agent_kind.trim();
    let meta = crate::agent::metadata_for(db, agent_kind)
        .await?
        .ok_or_else(|| anyhow!("unknown agent '{agent_kind}'"))?;
    crate::agent::validate_model(&meta, input.model.trim()).map_err(|e| anyhow!(e))?;
    crate::agent::validate_effort(&meta, input.effort.trim()).map_err(|e| anyhow!(e))?;
    let protocol = crate::agent::resolve_protocol(
        &meta,
        (!input.protocol.trim().is_empty()).then_some(input.protocol.trim()),
    )
    .map_err(|e| anyhow!(e))?;
    let mode = if input.mode.trim().is_empty() {
        crate::agent::DEFAULT_ACP_MODE.to_string()
    } else {
        input.mode.trim().to_string()
    };
    if !matches!(
        mode.as_str(),
        "auto" | "default" | "acceptEdits" | "plan" | "bypassPermissions"
    ) {
        bail!("invalid profile mode '{mode}'");
    }
    Ok((protocol, mode))
}

pub async fn active_count(db: &Db, name: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions
         WHERE profile = ? AND status NOT IN ('done', 'error', 'archived')",
    )
    .bind(name)
    .fetch_one(db)
    .await?)
}

pub async fn list(db: &Db) -> Result<Vec<Profile>> {
    Ok(
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles ORDER BY name")
            .fetch_all(db)
            .await?,
    )
}

pub async fn get(db: &Db, name: &str) -> Result<Option<Profile>> {
    Ok(
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ?")
            .bind(name)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn upsert(db: &Db, input: &ProfileInput) -> Result<Profile> {
    let name = input.name.trim();
    let (protocol, mode) = validate_input(db, input).await?;
    let ambient = serde_json::to_string(&input.ambient_allowlist)?;
    if let Some(existing) = get(db, name).await? {
        if existing.is_automation_safe()
            && has_automation_sessions(db, name).await?
            && (!input.strict
                || !input.env_clear
                || input.class != "automation"
                || widens_allowlist(&existing.ambient_names()?, &input.ambient_allowlist))
        {
            bail!("cannot weaken a profile referenced by automation sessions");
        }
    }
    let now = now_iso();
    sqlx::query(
        "INSERT INTO profiles
         (name, description, agent_kind, model, effort, protocol, mode, class,
          strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
          turn_budget, revision, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)
         ON CONFLICT(name) DO UPDATE SET
          description=excluded.description, agent_kind=excluded.agent_kind,
          model=excluded.model, effort=excluded.effort, protocol=excluded.protocol,
          mode=excluded.mode, class=excluded.class, strict=excluded.strict,
          env_clear=excluded.env_clear, ambient_allowlist=excluded.ambient_allowlist,
          idle_archive_secs=excluded.idle_archive_secs,
          max_concurrent=excluded.max_concurrent, turn_budget=excluded.turn_budget,
          revision=profiles.revision + 1, updated_at=excluded.updated_at",
    )
    .bind(name)
    .bind(input.description.trim())
    .bind(input.agent_kind.trim())
    .bind(input.model.trim())
    .bind(input.effort.trim())
    .bind(protocol)
    .bind(mode)
    .bind(input.class.trim())
    .bind(input.strict)
    .bind(input.env_clear)
    .bind(ambient)
    .bind(input.idle_archive_secs)
    .bind(input.max_concurrent)
    .bind(input.turn_budget)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    get(db, name)
        .await?
        .ok_or_else(|| anyhow!("profile vanished after upsert"))
}

fn widens_allowlist(old: &[String], new: &[String]) -> bool {
    new.iter().any(|name| !old.contains(name))
}

async fn has_automation_sessions(db: &Db, name: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE profile = ? AND class = 'automation')",
    )
    .bind(name)
    .fetch_one(db)
    .await?)
}

pub async fn remove(db: &Db, name: &str) -> Result<bool> {
    if name == DEFAULT_PROFILE {
        bail!("the default profile cannot be removed");
    }
    let referenced: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE profile = ?)")
            .bind(name)
            .fetch_one(db)
            .await?;
    if referenced {
        bail!("profile '{name}' is referenced by sessions");
    }
    Ok(sqlx::query("DELETE FROM profiles WHERE name = ?")
        .bind(name)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

pub async fn env_meta(db: &Db, profile: &str) -> Result<Vec<ProfileEnvMeta>> {
    Ok(sqlx::query_as::<_, ProfileEnvMeta>(
        "SELECT name, updated_at FROM profile_env WHERE profile_name = ? ORDER BY name",
    )
    .bind(profile)
    .fetch_all(db)
    .await?)
}

pub async fn env_pairs(db: &Db, profile: &str) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT name, value FROM profile_env WHERE profile_name = ? ORDER BY name",
    )
    .bind(profile)
    .fetch_all(db)
    .await?)
}

pub async fn env_get(db: &Db, profile: &str, name: &str) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT value FROM profile_env WHERE profile_name = ? AND name = ?")
            .bind(profile)
            .bind(name)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn env_set(db: &Db, profile: &str, name: &str, value: &str) -> Result<()> {
    crate::agent_env::validate_name(name).map_err(|e| anyhow!(e))?;
    if get(db, profile).await?.is_none() {
        bail!("unknown profile '{profile}'");
    }
    sqlx::query(
        "INSERT INTO profile_env (profile_name, name, value, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(profile_name, name) DO UPDATE SET
          value=excluded.value, updated_at=excluded.updated_at",
    )
    .bind(profile)
    .bind(name)
    .bind(value)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn env_remove(db: &Db, profile: &str, name: &str) -> Result<bool> {
    Ok(
        sqlx::query("DELETE FROM profile_env WHERE profile_name = ? AND name = ?")
            .bind(profile)
            .bind(name)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

/// Repair the one-time legacy seed through the same runtime metadata validators
/// new profile writes use. Valid profiles are left untouched; a stale removed
/// custom agent or selector falls back to the builtin default instead of making
/// every future launch fail after upgrade.
pub async fn normalize_default(db: &Db) -> Result<()> {
    let Some(current) = get(db, DEFAULT_PROFILE).await? else {
        bail!("profiles migration did not seed the default profile");
    };
    let input = ProfileInput {
        name: current.name.clone(),
        description: current.description.clone(),
        agent_kind: current.agent_kind.clone(),
        model: current.model.clone(),
        effort: current.effort.clone(),
        protocol: current.protocol.clone(),
        mode: current.mode.clone(),
        class: current.class.clone(),
        strict: current.strict,
        env_clear: current.env_clear,
        ambient_allowlist: current.ambient_names().unwrap_or_default(),
        idle_archive_secs: current.idle_archive_secs,
        max_concurrent: current.max_concurrent,
        turn_budget: current.turn_budget,
    };
    if validate_input(db, &input).await.is_ok() {
        return Ok(());
    }
    tracing::warn!(agent = %current.agent_kind, "repairing invalid legacy default profile");
    let fallback = ProfileInput {
        agent_kind: weaver_core::config::DEFAULT_AGENT.to_string(),
        model: String::new(),
        effort: String::new(),
        protocol: String::new(),
        mode: crate::agent::DEFAULT_ACP_MODE.to_string(),
        ..input
    };
    upsert(db, &fallback).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_are_portable() {
        assert!(validate_name("default").is_ok());
        assert!(validate_name("ops-cron_2").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("2bad").is_err());
        assert!(validate_name("bad name").is_err());
    }

    #[tokio::test]
    async fn env_values_are_separate_from_metadata() {
        let db = crate::db::connect_in_memory().await.unwrap();
        env_set(&db, DEFAULT_PROFILE, "API_TOKEN", "secret")
            .await
            .unwrap();
        assert_eq!(
            env_meta(&db, DEFAULT_PROFILE).await.unwrap()[0].name,
            "API_TOKEN"
        );
        assert_eq!(
            env_get(&db, DEFAULT_PROFILE, "API_TOKEN")
                .await
                .unwrap()
                .as_deref(),
            Some("secret")
        );
    }
}
