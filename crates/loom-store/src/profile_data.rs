//! Persistent profile records and launch helpers used below the policy layer.

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::db::Db;

pub use crate::agent_kind::DEFAULT_PROFILE;

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
    /// JSON array in storage; parsed through [`Profile::ambient_names`].
    pub ambient_allowlist: String,
    pub idle_archive_secs: Option<i64>,
    pub max_concurrent: i64,
    pub turn_budget: Option<i64>,
    pub prelude: String,
    pub restricted: bool,
    /// JSON array in storage; parsed through [`Profile::allowed_tool_rules`].
    pub allowed_tools: String,
    /// Provider-neutral MCP selection JSON.
    pub mcp_access: String,
    /// Exact resolved registry snapshot pinned to this profile revision.
    pub mcp_policy: String,
    /// Retired profiles remain only to support recovery of historical sessions.
    pub retired: bool,
    /// Stable identity for one selectable profile lifetime. Ordinary edits,
    /// environment rotation, and retirement preserve it; recreate advances it.
    pub lifetime: i64,
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

    pub fn mcp_access(&self) -> Result<weaver_api::McpAccess> {
        serde_json::from_str(&self.mcp_access).context("invalid profile MCP access")
    }

    pub fn mcp_policy_snapshot(&self) -> Result<weaver_api::McpPolicySnapshot> {
        serde_json::from_str(&self.mcp_policy).context("invalid profile MCP policy snapshot")
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
            mcp_access: self.mcp_access()?,
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
    #[serde(default, alias = "runtime_permissions")]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub mcp_access: weaver_api::McpAccess,
}

fn default_class() -> String {
    "interactive".to_string()
}

fn default_prelude() -> String {
    "weaver".to_string()
}

/// Extract the Claude SDK tool name from either `Read` or a scoped rule such
/// as `Bash(gh issue view:*)`.
pub fn allowed_tool_name(rule: &str) -> Option<&str> {
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

#[derive(Debug, Clone, sqlx::FromRow)]
struct ProfileEnvRow {
    name: String,
    value: String,
    source: String,
    secret_ref: Option<String>,
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
                let secret_ref = row.secret_ref.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("profile environment secret reference is missing")
                })?;
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

pub fn validate_gcp_secret_ref(secret_ref: &str) -> Result<()> {
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

/// Build the explicit ambient baseline used by an env-cleared profile.
pub fn cleared_environment(
    explicit: Vec<(String, String)>,
    ambient_allowlist: &[String],
) -> Vec<(String, String)> {
    const BASELINE: &[&str] = &[
        "PATH", "HOME", "SHELL", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "USER", "LOGNAME",
    ];
    let mut env = Vec::new();
    for name in BASELINE.iter().copied().chain(
        ambient_allowlist
            .iter()
            .map(String::as_str)
            .filter(|name| !BASELINE.contains(name)),
    ) {
        if let Ok(value) = std::env::var(name) {
            env.push((name.to_string(), value));
        }
    }
    crate::repo_env::layer(&mut env, explicit);
    env
}
