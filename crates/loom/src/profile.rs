//! Named, reusable session launch posture and environment.
//!
//! `default` is the compatibility boundary for the former flat `agent.*`
//! settings and `agent_env` table. New launches resolve one profile and stamp
//! its non-secret policy onto the session; profile environment values remain
//! rotatable and are loaded again on a real respawn.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
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
    pub prelude: String,
    pub restricted: bool,
    /// JSON array in storage; parsed through [`allowed_tool_rules`].
    pub allowed_tools: String,
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

    pub fn allowed_tool_rules(&self) -> Result<Vec<String>> {
        serde_json::from_str(&self.allowed_tools).context("invalid profile allowed tools")
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
            prelude: self.prelude.clone(),
            restricted: self.restricted,
            allowed_tools: self.allowed_tool_rules()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default = "default_prelude")]
    pub prelude: String,
    #[serde(default)]
    pub restricted: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

fn default_class() -> String {
    "interactive".to_string()
}

fn default_prelude() -> String {
    "weaver".to_string()
}

/// Extract the Claude SDK tool name from either `Read` or a scoped rule such
/// as `Bash(gh issue view:*)`. Reject malformed rules here so launch code never
/// has to guess how to build the adapter's visible-tools list.
pub(crate) fn allowed_tool_name(rule: &str) -> Option<&str> {
    if rule.is_empty() || rule != rule.trim() || rule.contains(['\n', '\r', '\0']) {
        return None;
    }
    if !rule.contains('(') {
        return Some(rule);
    }
    let body = rule.strip_suffix(')')?;
    let (name, pattern) = body.split_once('(')?;
    if name.is_empty() || pattern.is_empty() || pattern.contains(['(', ')']) {
        return None;
    }
    Some(name)
}

fn is_restricted_mcp_tool(rule: &str) -> bool {
    matches!(
        rule,
        "mcp__loom_github__issue_view"
            | "mcp__loom_github__issue_comment"
            | "mcp__loom_github__issue_edit"
            | "mcp__loom_github__pr_view"
            | "mcp__loom_github__pr_comment"
            | "mcp__loom_github__pr_edit"
    )
}

/// Restricted filesystem rules must stay below the session worktree. Claude's
/// permission syntax is glob-like, so require an explicit `./` anchor and reject
/// parent/root components before the rule ever reaches the adapter.
fn is_restricted_read_rule(rule: &str) -> bool {
    let Some(body) = rule.strip_suffix(')') else {
        return false;
    };
    let Some((name, pattern)) = body.split_once('(') else {
        return false;
    };
    matches!(name, "Read" | "Glob" | "Grep")
        && pattern.starts_with("./")
        && !pattern.contains('\\')
        && std::path::Path::new(pattern).components().all(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::Normal(_)
            )
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProfileEnvMeta {
    pub name: String,
    pub source: String,
    pub secret_ref: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ProfileEnvRow {
    name: String,
    value: String,
    source: String,
    secret_ref: Option<String>,
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
    if !matches!(input.prelude.trim(), "weaver" | "none") {
        bail!("profile prelude must be 'weaver' or 'none'");
    }
    if input.allowed_tools.len() > 64
        || input.allowed_tools.iter().any(|rule| {
            rule.len() > 256
                || !(matches!(
                    allowed_tool_name(rule),
                    Some("Read" | "Glob" | "Grep" | "Bash" | "WebFetch" | "WebSearch")
                ) || is_restricted_mcp_tool(rule))
        })
    {
        bail!("invalid profile allowed tool rule");
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
    if input.restricted
        && (input.class.trim() != "automation"
            || !input.strict
            || !input.env_clear
            || agent_kind != "claude"
            || protocol != "acp"
            || mode != "default"
            || input.prelude.trim() != "none"
            || input.allowed_tools.is_empty()
            || input
                .allowed_tools
                .iter()
                .any(|rule| !is_restricted_mcp_tool(rule) && !is_restricted_read_rule(rule))
            || !input.ambient_allowlist.is_empty())
    {
        bail!("restricted profiles must be strict env-cleared Claude ACP automation profiles with prelude 'none', mode 'default', no ambient allowlist, repository-scoped read rules, and/or fixed Loom GitHub MCP tools");
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
    let normalized = ProfileInput {
        name: name.to_string(),
        description: input.description.trim().to_string(),
        agent_kind: input.agent_kind.trim().to_string(),
        model: input.model.trim().to_string(),
        effort: input.effort.trim().to_string(),
        protocol,
        mode,
        class: input.class.trim().to_string(),
        strict: input.strict,
        env_clear: input.env_clear,
        ambient_allowlist: input.ambient_allowlist.clone(),
        idle_archive_secs: input.idle_archive_secs,
        max_concurrent: input.max_concurrent,
        turn_budget: input.turn_budget,
        prelude: input.prelude.trim().to_string(),
        restricted: input.restricted,
        allowed_tools: input.allowed_tools.clone(),
    };
    let ambient = serde_json::to_string(&normalized.ambient_allowlist)?;
    let allowed_tools = serde_json::to_string(&normalized.allowed_tools)?;
    if let Some(existing) = get(db, name).await? {
        if existing.as_input()? == normalized {
            return Ok(existing);
        }
        if existing.is_automation_safe()
            && has_automation_sessions(db, name).await?
            && (!normalized.strict
                || !normalized.env_clear
                || normalized.class != "automation"
                || (existing.restricted && !normalized.restricted)
                || (existing.restricted
                    && widens_allowlist(
                        &existing.allowed_tool_rules()?,
                        &normalized.allowed_tools,
                    ))
                || widens_allowlist(&existing.ambient_names()?, &normalized.ambient_allowlist))
        {
            bail!("cannot weaken a profile referenced by automation sessions");
        }
    }
    let now = now_iso();
    sqlx::query(
        "INSERT INTO profiles
         (name, description, agent_kind, model, effort, protocol, mode, class,
          strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
          turn_budget, revision, created_at, updated_at, prelude, restricted,
          allowed_tools)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET
          description=excluded.description, agent_kind=excluded.agent_kind,
          model=excluded.model, effort=excluded.effort, protocol=excluded.protocol,
          mode=excluded.mode, class=excluded.class, strict=excluded.strict,
          env_clear=excluded.env_clear, ambient_allowlist=excluded.ambient_allowlist,
          idle_archive_secs=excluded.idle_archive_secs,
          max_concurrent=excluded.max_concurrent, turn_budget=excluded.turn_budget,
          prelude=excluded.prelude, restricted=excluded.restricted,
          allowed_tools=excluded.allowed_tools,
          revision=profiles.revision + 1, updated_at=excluded.updated_at",
    )
    .bind(name)
    .bind(&normalized.description)
    .bind(&normalized.agent_kind)
    .bind(&normalized.model)
    .bind(&normalized.effort)
    .bind(&normalized.protocol)
    .bind(&normalized.mode)
    .bind(&normalized.class)
    .bind(normalized.strict)
    .bind(normalized.env_clear)
    .bind(ambient)
    .bind(normalized.idle_archive_secs)
    .bind(normalized.max_concurrent)
    .bind(normalized.turn_budget)
    .bind(&now)
    .bind(&now)
    .bind(&normalized.prelude)
    .bind(normalized.restricted)
    .bind(allowed_tools)
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
        "SELECT name, source, secret_ref, updated_at
         FROM profile_env WHERE profile_name = ? ORDER BY name",
    )
    .bind(profile)
    .fetch_all(db)
    .await?)
}

pub async fn env_pairs(db: &Db, profile: &str) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query_as::<_, ProfileEnvRow>(
        "SELECT name, value, source, secret_ref
         FROM profile_env WHERE profile_name = ? ORDER BY name",
    )
    .bind(profile)
    .fetch_all(db)
    .await?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let value = match row.source.as_str() {
            "literal" => row.value,
            "gcp_secret" => {
                let secret_ref = row
                    .secret_ref
                    .as_deref()
                    .ok_or_else(|| anyhow!("profile environment secret reference is missing"))?;
                resolve_gcp_secret(secret_ref).await.with_context(|| {
                    format!("resolving profile environment variable {}", row.name)
                })?
            }
            source => bail!("unsupported profile environment source '{source}'"),
        };
        values.push((row.name, value));
    }
    Ok(values)
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
        "INSERT INTO profile_env
         (profile_name, name, value, source, secret_ref, updated_at)
         VALUES (?, ?, ?, 'literal', NULL, ?)
         ON CONFLICT(profile_name, name) DO UPDATE SET
          value=excluded.value, source='literal', secret_ref=NULL,
          updated_at=excluded.updated_at",
    )
    .bind(profile)
    .bind(name)
    .bind(value)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn env_set_secret(db: &Db, profile: &str, name: &str, secret_ref: &str) -> Result<()> {
    crate::agent_env::validate_name(name).map_err(|e| anyhow!(e))?;
    if get(db, profile).await?.is_none() {
        bail!("unknown profile '{profile}'");
    }
    validate_gcp_secret_ref(secret_ref)?;
    sqlx::query(
        "INSERT INTO profile_env
         (profile_name, name, value, source, secret_ref, updated_at)
         VALUES (?, ?, '', 'gcp_secret', ?, ?)
         ON CONFLICT(profile_name, name) DO UPDATE SET
          value='', source='gcp_secret', secret_ref=excluded.secret_ref,
          updated_at=excluded.updated_at",
    )
    .bind(profile)
    .bind(name)
    .bind(secret_ref)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

fn validate_gcp_secret_ref(secret_ref: &str) -> Result<()> {
    let parts: Vec<&str> = secret_ref.split('/').collect();
    if parts.len() != 6
        || parts[0] != "projects"
        || parts[2] != "secrets"
        || parts[4] != "versions"
        || parts[1].is_empty()
        || parts[3].is_empty()
        || parts[5].is_empty()
        || !parts.iter().all(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        || (parts[5] != "latest" && !parts[5].bytes().all(|byte| byte.is_ascii_digit()))
    {
        bail!("secret_ref must be projects/PROJECT/secrets/SECRET/versions/VERSION");
    }
    Ok(())
}

async fn resolve_gcp_secret(secret_ref: &str) -> Result<String> {
    validate_gcp_secret_ref(secret_ref)?;
    #[derive(Deserialize)]
    struct MetadataToken {
        access_token: String,
    }
    #[derive(Deserialize)]
    struct SecretPayload {
        data: String,
    }
    #[derive(Deserialize)]
    struct SecretAccess {
        payload: SecretPayload,
    }

    let http = reqwest::Client::new();
    let token = http
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .context("requesting the VM workload access token")?
        .error_for_status()
        .context("the VM workload access token request was rejected")?
        .json::<MetadataToken>()
        .await
        .context("decoding the VM workload access token")?;
    let access = http
        .get(format!(
            "https://secretmanager.googleapis.com/v1/{secret_ref}:access"
        ))
        .bearer_auth(token.access_token)
        .send()
        .await
        .context("requesting the Secret Manager value")?
        .error_for_status()
        .context("the Secret Manager value request was rejected")?
        .json::<SecretAccess>()
        .await
        .context("decoding the Secret Manager response")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(access.payload.data)
        .context("decoding the Secret Manager payload")?;
    String::from_utf8(bytes).context("Secret Manager value is not UTF-8")
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

pub async fn mark_deployment_managed(db: &Db, name: &str) -> Result<()> {
    sqlx::query("UPDATE profiles SET managed_by_deployment = 1 WHERE name = ?")
        .bind(name)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn deployment_managed_names(db: &Db) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM profiles WHERE managed_by_deployment = 1 ORDER BY name",
    )
    .fetch_all(db)
    .await?)
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
        prelude: current.prelude.clone(),
        restricted: current.restricted,
        allowed_tools: current.allowed_tool_rules().unwrap_or_default(),
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

    #[test]
    fn claude_tool_rules_must_be_well_formed() {
        assert_eq!(allowed_tool_name("Read(./**)"), Some("Read"));
        assert_eq!(allowed_tool_name("Bash(gh issue view:*)"), Some("Bash"));
        assert_eq!(allowed_tool_name("Bash"), Some("Bash"));
        assert_eq!(allowed_tool_name("Bash(gh issue view:*"), None);
        assert_eq!(allowed_tool_name(" Bash(gh issue view:*)"), None);
        assert!(is_restricted_mcp_tool("mcp__loom_github__issue_edit"));
        assert!(!is_restricted_mcp_tool("mcp__other__issue_edit"));
    }

    #[tokio::test]
    async fn restricted_profiles_require_scoped_tool_rules() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let stock = get(&db, "github_comment").await.unwrap().unwrap();
        let mut input = stock.as_input().unwrap();
        input.allowed_tools = vec!["Read".to_string()];
        assert!(upsert(&db, &input).await.is_err());

        input.allowed_tools = vec!["Read(./**)".to_string()];
        assert!(upsert(&db, &input).await.is_ok());

        input.allowed_tools = vec!["Read(../**)".to_string()];
        assert!(upsert(&db, &input).await.is_err());
        input.allowed_tools = vec!["Glob(/etc/**)".to_string()];
        assert!(upsert(&db, &input).await.is_err());
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

    #[tokio::test]
    async fn unchanged_profiles_do_not_advance_the_revision() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let existing = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let input = existing.as_input().unwrap();

        let unchanged = upsert(&db, &input).await.unwrap();

        assert_eq!(unchanged.revision, existing.revision);
        assert_eq!(unchanged.updated_at, existing.updated_at);
    }

    #[tokio::test]
    async fn secret_references_are_validated_and_values_stay_out_of_the_database() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let secret_ref = "projects/acme-prod/secrets/ops_token/versions/latest";

        env_set_secret(&db, DEFAULT_PROFILE, "OPS_TOKEN", secret_ref)
            .await
            .unwrap();
        let metadata = env_meta(&db, DEFAULT_PROFILE).await.unwrap();
        assert_eq!(metadata[0].name, "OPS_TOKEN");
        assert_eq!(metadata[0].source, "gcp_secret");
        assert_eq!(metadata[0].secret_ref.as_deref(), Some(secret_ref));
        assert_eq!(
            env_get(&db, DEFAULT_PROFILE, "OPS_TOKEN")
                .await
                .unwrap()
                .as_deref(),
            Some("")
        );

        assert!(env_set_secret(
            &db,
            DEFAULT_PROFILE,
            "OPS_TOKEN",
            "projects/acme-prod/secrets/ops_token/versions/not-a-version"
        )
        .await
        .is_err());
    }
}
