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

const STOCK_PROFILES: &[(&str, &str)] = &[
    (
        "github_comment.json",
        include_str!("../profiles/github_comment.json"),
    ),
    ("watch.json", include_str!("../profiles/watch.json")),
];

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

    /// Exact rules to stamp onto a session. Profiles retain concise built-in
    /// MCP set names, but launched sessions are immutable and auditable.
    pub fn effective_allowed_tool_rules(&self) -> Result<Vec<String>> {
        let snapshot = self.mcp_policy_snapshot()?;
        self.effective_allowed_tool_rules_for(&snapshot)
    }

    pub fn effective_allowed_tool_rules_for(
        &self,
        snapshot: &weaver_api::McpPolicySnapshot,
    ) -> Result<Vec<String>> {
        let mut rules = crate::mcp::expand_tool_sets(&self.allowed_tool_rules()?)?;
        for rule in crate::mcp::rules_for_snapshot(snapshot)? {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
        Ok(rules)
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

fn is_restricted_mcp_tool_set(rule: &str) -> bool {
    crate::mcp::is_tool_set(rule)
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

async fn validate_input(
    db: &Db,
    input: &ProfileInput,
) -> Result<(String, String, weaver_api::McpPolicySnapshot)> {
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
                ) || is_restricted_mcp_tool_set(rule))
        })
    {
        bail!("invalid profile allowed tool rule");
    }
    let mcp_snapshot = crate::mcp::resolve_access(db, &input.mcp_access).await?;
    let custom_selected = crate::custom_mcp::list(db).await?.iter().any(|server| {
        input.mcp_access.mode == "all"
            || (input.mcp_access.mode == "groups"
                && input.mcp_access.groups.contains(&server.group))
    });
    if input.mcp_access.mode != "groups" && !input.mcp_access.groups.is_empty() {
        bail!("MCP groups may only be set when MCP access mode is 'groups'");
    }
    if input.mcp_access.mode == "groups" && input.mcp_access.groups.is_empty() {
        bail!("MCP access mode 'groups' requires at least one group");
    }
    if input.mcp_access.groups.len() > 64 {
        bail!("an MCP profile may select at most 64 groups");
    }
    let mut unique_groups = std::collections::HashSet::new();
    if input
        .mcp_access
        .groups
        .iter()
        .any(|group| group.len() > 64 || !unique_groups.insert(group))
    {
        bail!("MCP groups must be unique and at most 64 bytes");
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
    if protocol != "acp"
        && (input.mcp_access.mode != "none"
            || input
                .allowed_tools
                .iter()
                .any(|rule| is_restricted_mcp_tool_set(rule)))
    {
        bail!("MCP access requires the ACP protocol");
    }
    if input.restricted
        && (input.class.trim() != "automation"
            || !input.strict
            || !input.env_clear
            || agent_kind != "claude"
            || protocol != "acp"
            || mode != "default"
            || input.prelude.trim() != "none"
            || input.mcp_access.mode == "all"
            || (input.allowed_tools.is_empty() && mcp_snapshot.capability_sets.is_empty())
            || custom_selected
            || input
                .allowed_tools
                .iter()
                .any(|rule| !is_restricted_mcp_tool_set(rule) && !is_restricted_read_rule(rule))
            || !input.ambient_allowlist.is_empty())
    {
        bail!("restricted profiles must be strict env-cleared Claude ACP automation profiles with prelude 'none', mode 'default', no ambient allowlist, repository-scoped read rules, and/or reviewed built-in MCP tool sets");
    }
    Ok((protocol, mode, mcp_snapshot))
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

pub fn status_consumes_capacity(status: &str) -> bool {
    !matches!(status, "done" | "error" | "archived")
}

pub async fn list(db: &Db) -> Result<Vec<Profile>> {
    Ok(
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE retired = 0 ORDER BY name")
            .fetch_all(db)
            .await?,
    )
}

/// Include retired profiles when checking resources needed to recover sessions.
pub async fn list_including_retired(db: &Db) -> Result<Vec<Profile>> {
    Ok(
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles ORDER BY name")
            .fetch_all(db)
            .await?,
    )
}

pub async fn get(db: &Db, name: &str) -> Result<Option<Profile>> {
    Ok(
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ? AND retired = 0")
            .bind(name)
            .fetch_optional(db)
            .await?,
    )
}

/// Resolve a profile lifetime even after it has been retired. Session recovery
/// and flattened handoff use the stamped lifetime rather than requiring the
/// template to remain selectable for new launches.
pub async fn get_including_retired(db: &Db, name: &str) -> Result<Option<Profile>> {
    Ok(
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ?")
            .bind(name)
            .fetch_optional(db)
            .await?,
    )
}

pub struct PreparedProfile {
    normalized: ProfileInput,
    mcp_policy: weaver_api::McpPolicySnapshot,
}

pub async fn prepare_input(db: &Db, input: &ProfileInput) -> Result<PreparedProfile> {
    let (normalized, mcp_policy) = normalized_input(db, input).await?;
    Ok(PreparedProfile {
        normalized,
        mcp_policy,
    })
}

async fn normalized_input(
    db: &Db,
    input: &ProfileInput,
) -> Result<(ProfileInput, weaver_api::McpPolicySnapshot)> {
    let name = input.name.trim();
    let (protocol, mode, mcp_policy) = validate_input(db, input).await?;
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
        mcp_access: input.mcp_access.clone(),
    };
    Ok((normalized, mcp_policy))
}

pub enum CreateProfileOutcome {
    Created(Profile),
    Exists(Profile),
}

/// Insert one new selectable profile lifetime atomically. A retired tombstone
/// may be recreated under the same name, but its monotonic revision advances so
/// a preview from an earlier lifetime can never pass an optimistic guard.
pub async fn create(db: &Db, input: &ProfileInput) -> Result<CreateProfileOutcome> {
    let (normalized, mcp_policy) = normalized_input(db, input).await?;
    let name = normalized.name.as_str();
    let ambient = serde_json::to_string(&normalized.ambient_allowlist)?;
    let allowed_tools = serde_json::to_string(&normalized.allowed_tools)?;
    let mcp_access = serde_json::to_string(&normalized.mcp_access)?;
    let mcp_policy_json = serde_json::to_string(&mcp_policy)?;
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let existing = sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ?")
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(existing) = existing.as_ref().filter(|profile| !profile.retired) {
        tx.rollback().await?;
        return Ok(CreateProfileOutcome::Exists(existing.clone()));
    }

    if existing.is_some() {
        sqlx::query(
            "UPDATE profiles SET
             description = ?, agent_kind = ?, model = ?, effort = ?, protocol = ?,
             mode = ?, class = ?, strict = ?, env_clear = ?, ambient_allowlist = ?,
             idle_archive_secs = ?, max_concurrent = ?, turn_budget = ?, prelude = ?,
             restricted = ?, allowed_tools = ?, mcp_access = ?, mcp_policy = ?,
             retired = 0, lifetime = lifetime + 1,
             revision = revision + 1, updated_at = ?
             WHERE name = ? AND retired = 1",
        )
        .bind(&normalized.description)
        .bind(&normalized.agent_kind)
        .bind(&normalized.model)
        .bind(&normalized.effort)
        .bind(&normalized.protocol)
        .bind(&normalized.mode)
        .bind(&normalized.class)
        .bind(normalized.strict)
        .bind(normalized.env_clear)
        .bind(&ambient)
        .bind(normalized.idle_archive_secs)
        .bind(normalized.max_concurrent)
        .bind(normalized.turn_budget)
        .bind(&normalized.prelude)
        .bind(normalized.restricted)
        .bind(&allowed_tools)
        .bind(&mcp_access)
        .bind(&mcp_policy_json)
        .bind(&now)
        .bind(name)
        .execute(&mut *tx)
        .await?;
        // A recreated template starts with no write-only environment from the
        // unrelated retired lifetime.
        sqlx::query("DELETE FROM profile_env WHERE profile_name = ?")
            .bind(name)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO profiles
             (name, description, agent_kind, model, effort, protocol, mode, class,
              strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
              turn_budget, revision, created_at, updated_at, prelude, restricted,
              allowed_tools, mcp_access, mcp_policy)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&ambient)
        .bind(normalized.idle_archive_secs)
        .bind(normalized.max_concurrent)
        .bind(normalized.turn_budget)
        .bind(&now)
        .bind(&now)
        .bind(&normalized.prelude)
        .bind(normalized.restricted)
        .bind(&allowed_tools)
        .bind(&mcp_access)
        .bind(&mcp_policy_json)
        .execute(&mut *tx)
        .await?;
    }
    let created =
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ? AND retired = 0")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(CreateProfileOutcome::Created(created))
}

pub async fn upsert(db: &Db, input: &ProfileInput) -> Result<Profile> {
    let (normalized, mcp_policy) = normalized_input(db, input).await?;
    let name = normalized.name.as_str();
    let ambient = serde_json::to_string(&normalized.ambient_allowlist)?;
    let allowed_tools = serde_json::to_string(&normalized.allowed_tools)?;
    let mcp_access = serde_json::to_string(&normalized.mcp_access)?;
    let mcp_policy_json = serde_json::to_string(&mcp_policy)?;
    if let Some(existing) = get(db, name).await? {
        if existing.as_input()? == normalized && existing.mcp_policy_snapshot()? == mcp_policy {
            return Ok(existing);
        }
        let widens_restricted_tools = existing.restricted
            && widens_allowlist(
                &existing.effective_allowed_tool_rules_for(&existing.mcp_policy_snapshot()?)?,
                &{
                    let mut rules = crate::mcp::expand_tool_sets(&normalized.allowed_tools)?;
                    rules.extend(crate::mcp::rules_for_snapshot(&mcp_policy)?);
                    rules
                },
            );
        if existing.is_automation_safe()
            && has_automation_sessions(db, name).await?
            && (!normalized.strict
                || !normalized.env_clear
                || normalized.class != "automation"
                || (existing.restricted && !normalized.restricted)
                || widens_restricted_tools
                || widens_allowlist(&existing.ambient_names()?, &normalized.ambient_allowlist))
        {
            bail!("cannot weaken a profile referenced by automation sessions");
        }
    }
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let reviving_retired: bool = sqlx::query_scalar("SELECT retired FROM profiles WHERE name = ?")
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(false);
    sqlx::query(
        "INSERT INTO profiles
         (name, description, agent_kind, model, effort, protocol, mode, class,
          strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
          turn_budget, revision, created_at, updated_at, prelude, restricted,
          allowed_tools, mcp_access, mcp_policy)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET
          description=excluded.description, agent_kind=excluded.agent_kind,
          model=excluded.model, effort=excluded.effort, protocol=excluded.protocol,
          mode=excluded.mode, class=excluded.class, strict=excluded.strict,
          env_clear=excluded.env_clear, ambient_allowlist=excluded.ambient_allowlist,
          idle_archive_secs=excluded.idle_archive_secs,
          max_concurrent=excluded.max_concurrent, turn_budget=excluded.turn_budget,
          prelude=excluded.prelude, restricted=excluded.restricted,
          allowed_tools=excluded.allowed_tools, mcp_access=excluded.mcp_access,
          mcp_policy=excluded.mcp_policy, retired=0,
          lifetime=CASE
            WHEN profiles.retired = 1 THEN profiles.lifetime + 1
            ELSE profiles.lifetime
          END,
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
    .bind(mcp_access)
    .bind(mcp_policy_json)
    .execute(&mut *tx)
    .await?;
    if reviving_retired {
        // Every revival is a new lifetime. Tombstoned credentials belong to
        // the previous lifetime and may only return through an explicit new
        // environment proposal.
        sqlx::query("DELETE FROM profile_env WHERE profile_name = ?")
            .bind(name)
            .execute(&mut *tx)
            .await?;
    }
    let updated =
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ? AND retired = 0")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(updated)
}

pub enum UpdateProfileOutcome {
    Updated(Profile),
    Stale(Profile),
    Missing,
}

/// Replace an existing profile under an atomic revision guard. Environment
/// edits use the same profile revision, so a settings form can never overwrite
/// a template whose write-only inputs changed after it loaded.
pub async fn update_expected(
    db: &Db,
    input: &ProfileInput,
    expected_revision: i64,
) -> Result<UpdateProfileOutcome> {
    let (normalized, mcp_policy) = normalized_input(db, input).await?;
    let name = normalized.name.as_str();
    let ambient = serde_json::to_string(&normalized.ambient_allowlist)?;
    let allowed_tools = serde_json::to_string(&normalized.allowed_tools)?;
    let mcp_access = serde_json::to_string(&normalized.mcp_access)?;
    let mcp_policy_json = serde_json::to_string(&mcp_policy)?;
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let Some(existing) =
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ? AND retired = 0")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
    else {
        tx.rollback().await?;
        return Ok(UpdateProfileOutcome::Missing);
    };
    if existing.revision != expected_revision {
        tx.rollback().await?;
        return Ok(UpdateProfileOutcome::Stale(existing));
    }
    if existing.as_input()? == normalized && existing.mcp_policy_snapshot()? == mcp_policy {
        tx.commit().await?;
        return Ok(UpdateProfileOutcome::Updated(existing));
    }
    let widens_restricted_tools = existing.restricted
        && widens_allowlist(
            &existing.effective_allowed_tool_rules_for(&existing.mcp_policy_snapshot()?)?,
            &{
                let mut rules = crate::mcp::expand_tool_sets(&normalized.allowed_tools)?;
                rules.extend(crate::mcp::rules_for_snapshot(&mcp_policy)?);
                rules
            },
        );
    let has_automation: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE profile = ? AND class = 'automation')",
    )
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    if existing.is_automation_safe()
        && has_automation
        && (!normalized.strict
            || !normalized.env_clear
            || normalized.class != "automation"
            || (existing.restricted && !normalized.restricted)
            || widens_restricted_tools
            || widens_allowlist(&existing.ambient_names()?, &normalized.ambient_allowlist))
    {
        bail!("cannot weaken a profile referenced by automation sessions");
    }
    sqlx::query(
        "UPDATE profiles SET
         description = ?, agent_kind = ?, model = ?, effort = ?, protocol = ?,
         mode = ?, class = ?, strict = ?, env_clear = ?, ambient_allowlist = ?,
         idle_archive_secs = ?, max_concurrent = ?, turn_budget = ?, prelude = ?,
         restricted = ?, allowed_tools = ?, mcp_access = ?, mcp_policy = ?,
         retired = 0, revision = revision + 1, updated_at = ?
         WHERE name = ? AND revision = ?",
    )
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
    .bind(&normalized.prelude)
    .bind(normalized.restricted)
    .bind(allowed_tools)
    .bind(mcp_access)
    .bind(mcp_policy_json)
    .bind(&now)
    .bind(name)
    .bind(expected_revision)
    .execute(&mut *tx)
    .await?;
    let updated =
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ? AND retired = 0")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(UpdateProfileOutcome::Updated(updated))
}

pub enum CloneProfileOutcome {
    Created(Profile),
    Stale(Profile),
    TargetExists,
}

/// Create a profile from a server-owned source snapshot and optionally copy its
/// write-only environment in the same SQLite transaction. The source revision
/// guard is checked inside that transaction, so an environment or template edit
/// cannot race between preview and clone.
#[cfg(test)]
async fn create_clone(
    db: &Db,
    source_name: &str,
    expected_source_revision: i64,
    input: &ProfileInput,
    copy_environment: bool,
) -> Result<CloneProfileOutcome> {
    let prepared = prepare_input(db, input).await?;
    create_clone_prepared(
        db,
        source_name,
        expected_source_revision,
        prepared,
        &weaver_api::CloneProfileEnvironmentReq {
            inherit: copy_environment,
            ..Default::default()
        },
    )
    .await
}

/// Commit an already normalized clone proposal and its environment edits in
/// one transaction. The caller owns the resolver-generation fence from
/// [`prepare_input`] through this function.
pub async fn create_clone_prepared(
    db: &Db,
    source_name: &str,
    expected_source_revision: i64,
    prepared: PreparedProfile,
    environment: &weaver_api::CloneProfileEnvironmentReq,
) -> Result<CloneProfileOutcome> {
    let PreparedProfile {
        normalized,
        mcp_policy: current_mcp_policy,
    } = prepared;
    let mut seen_remove = std::collections::HashSet::new();
    for name in &environment.remove {
        crate::agent_env::validate_name(name).map_err(|error| anyhow!(error))?;
        if !seen_remove.insert(name.as_str()) {
            bail!("duplicate environment removal '{name}'");
        }
    }
    let mut seen_set = std::collections::HashSet::new();
    for entry in &environment.set {
        crate::agent_env::validate_name(&entry.name).map_err(|error| anyhow!(error))?;
        if !seen_set.insert(entry.name.as_str()) {
            bail!("duplicate environment value '{}'", entry.name);
        }
        match (&entry.value, &entry.secret_ref) {
            (Some(_), None) => {}
            (None, Some(secret_ref)) => validate_gcp_secret_ref(secret_ref)?,
            _ => bail!(
                "environment '{}' requires exactly one of value and secret_ref",
                entry.name
            ),
        }
    }
    let target_name = normalized.name.as_str();
    let ambient = serde_json::to_string(&normalized.ambient_allowlist)?;
    let allowed_tools = serde_json::to_string(&normalized.allowed_tools)?;
    let mcp_access = serde_json::to_string(&normalized.mcp_access)?;
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let Some(source) =
        sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ? AND retired = 0")
            .bind(source_name)
            .fetch_optional(&mut *tx)
            .await?
    else {
        bail!("unknown profile '{source_name}'");
    };
    if source.revision != expected_source_revision {
        tx.rollback().await?;
        return Ok(CloneProfileOutcome::Stale(source));
    }
    let target = sqlx::query_as::<_, Profile>("SELECT * FROM profiles WHERE name = ?")
        .bind(target_name)
        .fetch_optional(&mut *tx)
        .await?;
    if target.as_ref().is_some_and(|profile| !profile.retired) {
        tx.rollback().await?;
        return Ok(CloneProfileOutcome::TargetExists);
    }

    // `normalized_input` resolved the submitted template against the current
    // registry before this transaction. The clone route guards that resolver
    // fingerprint, so the snapshot written here is exactly the composition the
    // caller reviewed (including edits made in the reusable editor).
    if target.is_some() {
        sqlx::query(
            "UPDATE profiles SET
             description = ?, agent_kind = ?, model = ?, effort = ?, protocol = ?,
             mode = ?, class = ?, strict = ?, env_clear = ?, ambient_allowlist = ?,
             idle_archive_secs = ?, max_concurrent = ?, turn_budget = ?, prelude = ?,
             restricted = ?, allowed_tools = ?, mcp_access = ?, mcp_policy = ?,
             retired = 0, lifetime = lifetime + 1,
             revision = revision + 1, updated_at = ?
             WHERE name = ? AND retired = 1",
        )
        .bind(&normalized.description)
        .bind(&normalized.agent_kind)
        .bind(&normalized.model)
        .bind(&normalized.effort)
        .bind(&normalized.protocol)
        .bind(&normalized.mode)
        .bind(&normalized.class)
        .bind(normalized.strict)
        .bind(normalized.env_clear)
        .bind(&ambient)
        .bind(normalized.idle_archive_secs)
        .bind(normalized.max_concurrent)
        .bind(normalized.turn_budget)
        .bind(&normalized.prelude)
        .bind(normalized.restricted)
        .bind(&allowed_tools)
        .bind(&mcp_access)
        .bind(serde_json::to_string(&current_mcp_policy)?)
        .bind(&now)
        .bind(target_name)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM profile_env WHERE profile_name = ?")
            .bind(target_name)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO profiles
             (name, description, agent_kind, model, effort, protocol, mode, class,
              strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
              turn_budget, revision, created_at, updated_at, prelude, restricted,
              allowed_tools, mcp_access, mcp_policy)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(target_name)
        .bind(&normalized.description)
        .bind(&normalized.agent_kind)
        .bind(&normalized.model)
        .bind(&normalized.effort)
        .bind(&normalized.protocol)
        .bind(&normalized.mode)
        .bind(&normalized.class)
        .bind(normalized.strict)
        .bind(normalized.env_clear)
        .bind(&ambient)
        .bind(normalized.idle_archive_secs)
        .bind(normalized.max_concurrent)
        .bind(normalized.turn_budget)
        .bind(&now)
        .bind(&now)
        .bind(&normalized.prelude)
        .bind(normalized.restricted)
        .bind(allowed_tools)
        .bind(mcp_access)
        .bind(serde_json::to_string(&current_mcp_policy)?)
        .execute(&mut *tx)
        .await?;
    }
    if environment.inherit {
        sqlx::query(
            "INSERT INTO profile_env
             (profile_name, name, value, source, secret_ref, updated_at)
             SELECT ?, name, value, source, secret_ref, ?
             FROM profile_env
             WHERE profile_name = ?",
        )
        .bind(target_name)
        .bind(&now)
        .bind(source_name)
        .execute(&mut *tx)
        .await?;
    }
    for name in &environment.remove {
        sqlx::query("DELETE FROM profile_env WHERE profile_name = ? AND name = ?")
            .bind(target_name)
            .bind(name)
            .execute(&mut *tx)
            .await?;
    }
    for entry in &environment.set {
        let (value, source, secret_ref) = match (&entry.value, &entry.secret_ref) {
            (Some(value), None) => (value.as_str(), "literal", None),
            (None, Some(secret_ref)) => ("", "gcp_secret", Some(secret_ref.as_str())),
            _ => unreachable!("environment proposal was validated"),
        };
        sqlx::query(
            "INSERT INTO profile_env
             (profile_name, name, value, source, secret_ref, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(profile_name, name) DO UPDATE SET
              value=excluded.value, source=excluded.source,
              secret_ref=excluded.secret_ref, updated_at=excluded.updated_at",
        )
        .bind(target_name)
        .bind(&entry.name)
        .bind(value)
        .bind(source)
        .bind(secret_ref)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let created = get(db, target_name)
        .await?
        .ok_or_else(|| anyhow!("cloned profile vanished after create"))?;
    Ok(CloneProfileOutcome::Created(created))
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
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let watch_referenced: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM watches WHERE profile = ?)")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
    if watch_referenced {
        bail!("profile '{name}' is selected by watches");
    }
    let active_session_referenced: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM sessions
            WHERE profile = ? AND status NOT IN ('done', 'error', 'archived')
        )",
    )
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    if active_session_referenced {
        bail!("profile '{name}' is referenced by non-terminal sessions");
    }
    let changed = sqlx::query(
        "UPDATE profiles
         SET retired = 1, revision = revision + 1, updated_at = ?
         WHERE name = ? AND retired = 0",
    )
    .bind(now_iso())
    .bind(name)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    tx.commit().await?;
    Ok(changed)
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

/// Build the explicit ambient baseline used by an env-cleared profile.
/// Profile-owned values win over the small process baseline and any explicitly
/// allowlisted ambient names.
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
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let changed = sqlx::query(
        "INSERT INTO profile_env
         (profile_name, name, value, source, secret_ref, updated_at)
         VALUES (?, ?, ?, 'literal', NULL, ?)
         ON CONFLICT(profile_name, name) DO UPDATE SET
          value=excluded.value, source='literal', secret_ref=NULL,
          updated_at=excluded.updated_at
         WHERE profile_env.value != excluded.value
            OR profile_env.source != 'literal'
            OR profile_env.secret_ref IS NOT NULL",
    )
    .bind(profile)
    .bind(name)
    .bind(value)
    .bind(&now)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if changed {
        sqlx::query("UPDATE profiles SET revision = revision + 1, updated_at = ? WHERE name = ?")
            .bind(&now)
            .bind(profile)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn env_set_secret(db: &Db, profile: &str, name: &str, secret_ref: &str) -> Result<()> {
    crate::agent_env::validate_name(name).map_err(|e| anyhow!(e))?;
    if get(db, profile).await?.is_none() {
        bail!("unknown profile '{profile}'");
    }
    validate_gcp_secret_ref(secret_ref)?;
    let now = now_iso();
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let changed = sqlx::query(
        "INSERT INTO profile_env
         (profile_name, name, value, source, secret_ref, updated_at)
         VALUES (?, ?, '', 'gcp_secret', ?, ?)
         ON CONFLICT(profile_name, name) DO UPDATE SET
          value='', source='gcp_secret', secret_ref=excluded.secret_ref,
          updated_at=excluded.updated_at
         WHERE profile_env.value != ''
            OR profile_env.source != 'gcp_secret'
            OR NOT (profile_env.secret_ref IS excluded.secret_ref)",
    )
    .bind(profile)
    .bind(name)
    .bind(secret_ref)
    .bind(&now)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if changed {
        sqlx::query("UPDATE profiles SET revision = revision + 1, updated_at = ? WHERE name = ?")
            .bind(&now)
            .bind(profile)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
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
    let mut tx = weaver_core::db::begin_immediate(db).await?;
    let removed = sqlx::query("DELETE FROM profile_env WHERE profile_name = ? AND name = ?")
        .bind(profile)
        .bind(name)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if removed {
        sqlx::query("UPDATE profiles SET revision = revision + 1, updated_at = ? WHERE name = ?")
            .bind(now_iso())
            .bind(profile)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(removed)
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

/// Seed reviewed stock profile manifests only when no lifetime has ever existed
/// under that name. Existing rows remain operator-editable, and an explicit
/// tombstone is a durable opt-out rather than first-run absence.
pub async fn seed_stock_profiles(db: &Db) -> Result<()> {
    for (source, contents) in STOCK_PROFILES {
        let input: ProfileInput = serde_json::from_str(contents)
            .with_context(|| format!("parsing stock profile {source}"))?;
        if get_including_retired(db, &input.name).await?.is_none() {
            upsert(db, &input)
                .await
                .with_context(|| format!("seeding stock profile {source}"))?;
        }
    }
    Ok(())
}

/// Populate the exact MCP snapshot for profiles created before migration 6.
/// This is a compatibility backfill, so it does not advance their revision.
pub async fn backfill_mcp_policies(db: &Db) -> Result<()> {
    for profile in list(db).await? {
        if !profile.mcp_policy.is_empty() {
            continue;
        }
        let snapshot = crate::mcp::resolve_access(db, &profile.mcp_access()?).await?;
        sqlx::query("UPDATE profiles SET mcp_policy = ? WHERE name = ? AND mcp_policy = ''")
            .bind(serde_json::to_string(&snapshot)?)
            .bind(&profile.name)
            .execute(db)
            .await?;
    }
    Ok(())
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
        mcp_access: current.mcp_access().unwrap_or_default(),
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
        assert!(is_restricted_mcp_tool_set("mcp/github/comment@v1"));
        assert!(!is_restricted_mcp_tool_set("mcp/github/admin"));
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

        input.allowed_tools = vec!["mcp/github/comment@v1".to_string()];
        assert!(upsert(&db, &input).await.is_ok());

        input.allowed_tools = vec!["Read(../**)".to_string()];
        assert!(upsert(&db, &input).await.is_err());
        input.allowed_tools = vec!["Glob(/etc/**)".to_string()];
        assert!(upsert(&db, &input).await.is_err());

        input.allowed_tools = vec!["Read(./**)".to_string()];
        input.mcp_access = weaver_api::McpAccess {
            mode: "all".to_string(),
            groups: Vec::new(),
        };
        assert!(upsert(&db, &input).await.is_err());
    }

    #[tokio::test]
    async fn mcp_selection_requires_groups_and_acp() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut input = get(&db, DEFAULT_PROFILE)
            .await
            .unwrap()
            .unwrap()
            .as_input()
            .unwrap();
        input.mcp_access = weaver_api::McpAccess {
            mode: "groups".to_string(),
            groups: Vec::new(),
        };
        assert!(upsert(&db, &input).await.is_err());

        input.mcp_access.groups = vec!["github".to_string()];
        input.protocol = "terminal".to_string();
        assert!(upsert(&db, &input).await.is_err());
    }

    #[tokio::test]
    async fn stock_profiles_seed_from_manifests_without_overwriting_edits() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let stock = get(&db, "github_comment").await.unwrap().unwrap();
        assert!(stock.restricted);
        let watch = get(&db, "watch").await.unwrap().unwrap();
        assert_eq!(watch.agent_kind, "codex");
        assert_eq!(watch.model, "gpt-5.6-sol");
        assert_eq!(watch.effort, "medium");
        assert_eq!(watch.mode, "plan");
        assert!(watch.is_automation_safe());
        assert!(!watch.restricted);

        let mut edited = stock.as_input().unwrap();
        edited.description = "operator-edited description".to_string();
        upsert(&db, &edited).await.unwrap();
        seed_stock_profiles(&db).await.unwrap();

        assert_eq!(
            get(&db, "github_comment")
                .await
                .unwrap()
                .unwrap()
                .description,
            "operator-edited description"
        );

        env_set(&db, "github_comment", "STOCK_SECRET", "write-only")
            .await
            .unwrap();
        let before_delete = get(&db, "github_comment").await.unwrap().unwrap();
        remove(&db, "github_comment").await.unwrap();
        seed_stock_profiles(&db).await.unwrap();
        assert!(
            get(&db, "github_comment").await.unwrap().is_none(),
            "an explicitly deleted stock profile stays unselectable after restart seeding"
        );
        let tombstone = get_including_retired(&db, "github_comment")
            .await
            .unwrap()
            .unwrap();
        assert!(tombstone.retired);
        assert_eq!(tombstone.lifetime, before_delete.lifetime);
        assert_eq!(
            env_get(&db, "github_comment", "STOCK_SECRET")
                .await
                .unwrap()
                .as_deref(),
            Some("write-only"),
            "startup does not revive or clear a tombstoned lifetime"
        );
    }

    #[tokio::test]
    async fn env_values_are_separate_from_metadata() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let before = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision;
        env_set(&db, DEFAULT_PROFILE, "API_TOKEN", "secret")
            .await
            .unwrap();
        let after_set = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision;
        assert_eq!(after_set, before + 1);
        env_set(&db, DEFAULT_PROFILE, "API_TOKEN", "secret")
            .await
            .unwrap();
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            after_set,
            "replaying the same literal value is not a template edit"
        );
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
        env_set_secret(
            &db,
            DEFAULT_PROFILE,
            "API_TOKEN",
            "projects/test/secrets/token/versions/1",
        )
        .await
        .unwrap();
        let after_secret = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision;
        assert_eq!(after_secret, after_set + 1);
        env_set_secret(
            &db,
            DEFAULT_PROFILE,
            "API_TOKEN",
            "projects/test/secrets/token/versions/1",
        )
        .await
        .unwrap();
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            after_secret,
            "replaying the same secret reference is not a template edit"
        );
        assert!(env_remove(&db, DEFAULT_PROFILE, "API_TOKEN").await.unwrap());
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            after_secret + 1
        );
        assert!(!env_remove(&db, DEFAULT_PROFILE, "API_TOKEN").await.unwrap());
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            after_secret + 1,
            "a no-op removal is not a template edit"
        );
    }

    #[tokio::test]
    async fn stale_clone_after_environment_edit_creates_nothing() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let source = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let mut clone = source.as_input().unwrap();
        clone.name = "default-copy".to_string();
        env_set(&db, DEFAULT_PROFILE, "TOKEN", "changed")
            .await
            .unwrap();

        let outcome = create_clone(&db, DEFAULT_PROFILE, source.revision, &clone, true)
            .await
            .unwrap();
        assert!(matches!(outcome, CloneProfileOutcome::Stale(_)));
        assert!(get(&db, "default-copy").await.unwrap().is_none());
        assert!(env_meta(&db, "default-copy").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn optimistic_profile_update_observes_environment_revisions() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let source = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let mut edited = source.as_input().unwrap();
        edited.description = "stale editor".to_string();
        env_set(&db, DEFAULT_PROFILE, "TOKEN", "new input")
            .await
            .unwrap();
        let literal_revision = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision;
        assert_eq!(literal_revision, source.revision + 1);
        env_set(&db, DEFAULT_PROFILE, "TOKEN", "new input")
            .await
            .unwrap();
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            literal_revision,
            "an identical literal write is not a new template input"
        );
        env_set_secret(
            &db,
            DEFAULT_PROFILE,
            "TOKEN",
            "projects/p/secrets/token/versions/latest",
        )
        .await
        .unwrap();
        let secret_revision = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision;
        assert_eq!(secret_revision, literal_revision + 1);
        env_set_secret(
            &db,
            DEFAULT_PROFILE,
            "TOKEN",
            "projects/p/secrets/token/versions/latest",
        )
        .await
        .unwrap();
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            secret_revision,
            "an identical secret reference is not a new template input"
        );
        assert!(env_remove(&db, DEFAULT_PROFILE, "TOKEN").await.unwrap());
        let removed_revision = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision;
        assert_eq!(removed_revision, secret_revision + 1);
        assert!(!env_remove(&db, DEFAULT_PROFILE, "TOKEN").await.unwrap());
        assert_eq!(
            get(&db, DEFAULT_PROFILE).await.unwrap().unwrap().revision,
            removed_revision,
            "removing a missing environment name is a no-op"
        );

        match update_expected(&db, &edited, source.revision)
            .await
            .unwrap()
        {
            UpdateProfileOutcome::Stale(current) => {
                assert_eq!(current.revision, source.revision + 3)
            }
            _ => panic!("environment edit must make the profile editor stale"),
        }
        assert_ne!(
            get(&db, DEFAULT_PROFILE)
                .await
                .unwrap()
                .unwrap()
                .description,
            "stale editor"
        );
    }

    #[tokio::test]
    async fn clone_composes_environment_inside_created_profile() {
        let db = crate::db::connect_in_memory().await.unwrap();
        env_set(&db, DEFAULT_PROFILE, "TOKEN", "secret")
            .await
            .unwrap();
        env_set(&db, DEFAULT_PROFILE, "REMOVE_ME", "discarded")
            .await
            .unwrap();
        let source = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let mut clone = source.as_input().unwrap();
        clone.name = "default-copy".to_string();
        let prepared = prepare_input(&db, &clone).await.unwrap();

        let outcome = create_clone_prepared(
            &db,
            DEFAULT_PROFILE,
            source.revision,
            prepared,
            &weaver_api::CloneProfileEnvironmentReq {
                inherit: true,
                remove: vec!["REMOVE_ME".to_string()],
                set: vec![
                    weaver_api::ProfileEnvMutationReq {
                        name: "TOKEN".to_string(),
                        value: Some("replaced".to_string()),
                        secret_ref: None,
                    },
                    weaver_api::ProfileEnvMutationReq {
                        name: "NEW_SECRET".to_string(),
                        value: None,
                        secret_ref: Some("projects/acme/secrets/new/versions/latest".to_string()),
                    },
                ],
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, CloneProfileOutcome::Created(_)));
        assert_eq!(
            env_get(&db, "default-copy", "TOKEN")
                .await
                .unwrap()
                .as_deref(),
            Some("replaced")
        );
        assert!(env_get(&db, "default-copy", "REMOVE_ME")
            .await
            .unwrap()
            .is_none());
        let metadata = env_meta(&db, "default-copy").await.unwrap();
        assert_eq!(
            metadata
                .iter()
                .find(|entry| entry.name == "NEW_SECRET")
                .and_then(|entry| entry.secret_ref.as_deref()),
            Some("projects/acme/secrets/new/versions/latest")
        );
    }

    #[tokio::test]
    async fn clone_environment_failure_rolls_back_the_profile() {
        let db = crate::db::connect_in_memory().await.unwrap();
        env_set(&db, DEFAULT_PROFILE, "TOKEN", "secret")
            .await
            .unwrap();
        let source = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let mut clone = source.as_input().unwrap();
        clone.name = "copy-must-rollback".to_string();
        sqlx::query(
            "CREATE TRIGGER reject_clone_environment
             BEFORE INSERT ON profile_env
             WHEN NEW.profile_name = 'copy-must-rollback'
             BEGIN
               SELECT RAISE(ABORT, 'injected environment copy failure');
             END",
        )
        .execute(&db)
        .await
        .unwrap();

        assert!(
            create_clone(&db, DEFAULT_PROFILE, source.revision, &clone, true)
                .await
                .is_err()
        );
        assert!(get_including_retired(&db, "copy-must-rollback")
            .await
            .unwrap()
            .is_none());
        assert!(env_meta(&db, "copy-must-rollback")
            .await
            .unwrap()
            .is_empty());
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
    async fn profiles_selected_by_watches_cannot_be_removed() {
        let db = crate::db::connect_in_memory().await.unwrap();
        weaver_core::watch::create(
            &db,
            &weaver_core::watch::NewWatch {
                name: "profile-owner".to_string(),
                profile: "github_comment".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert!(remove(&db, "github_comment").await.is_err());
    }

    #[tokio::test]
    async fn atomic_create_allows_exactly_one_writer() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut input = get(&db, DEFAULT_PROFILE)
            .await
            .unwrap()
            .unwrap()
            .as_input()
            .unwrap();
        input.name = "atomic-create".to_string();
        let left_db = db.clone();
        let left_input = input.clone();
        let right_db = db.clone();
        let (left, right) = tokio::join!(
            async move { create(&left_db, &left_input).await.unwrap() },
            async move { create(&right_db, &input).await.unwrap() },
        );
        let created = [&left, &right]
            .into_iter()
            .filter(|outcome| matches!(outcome, CreateProfileOutcome::Created(_)))
            .count();
        let exists = [&left, &right]
            .into_iter()
            .filter(|outcome| matches!(outcome, CreateProfileOutcome::Exists(_)))
            .count();
        assert_eq!((created, exists), (1, 1));
    }

    #[tokio::test]
    async fn retired_name_recreation_advances_lifetime_revision() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut input = get(&db, DEFAULT_PROFILE)
            .await
            .unwrap()
            .unwrap()
            .as_input()
            .unwrap();
        input.name = "recreated".to_string();
        match create(&db, &input).await.unwrap() {
            CreateProfileOutcome::Created(_) => {}
            CreateProfileOutcome::Exists(_) => panic!("new name unexpectedly existed"),
        }
        env_set(&db, &input.name, "OLD_SECRET", "previous-lifetime")
            .await
            .unwrap();
        let first = get(&db, &input.name).await.unwrap().unwrap();
        assert!(remove(&db, &input.name).await.unwrap());
        let tombstone = get_including_retired(&db, &input.name)
            .await
            .unwrap()
            .unwrap();
        assert!(tombstone.retired);
        assert_eq!(tombstone.revision, first.revision + 1);
        assert_eq!(tombstone.lifetime, first.lifetime);
        assert_eq!(
            env_get(&db, &input.name, "OLD_SECRET")
                .await
                .unwrap()
                .as_deref(),
            Some("previous-lifetime"),
            "retirement preserves credentials for same-lifetime recovery"
        );

        input.description = "unrelated replacement".to_string();
        let replacement = match create(&db, &input).await.unwrap() {
            CreateProfileOutcome::Created(profile) => profile,
            CreateProfileOutcome::Exists(_) => panic!("retired name was not recreated"),
        };
        assert_eq!(replacement.revision, tombstone.revision + 1);
        assert_ne!(replacement.revision, first.revision);
        assert_eq!(replacement.lifetime, first.lifetime + 1);
        assert!(
            env_meta(&db, &input.name).await.unwrap().is_empty(),
            "recreate must not revive tombstoned credentials"
        );
    }

    #[tokio::test]
    async fn every_retired_revival_advances_lifetime_and_clears_environment() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let source = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let mut input = source.as_input().unwrap();
        input.name = "revive-upsert".to_string();
        let created = match create(&db, &input).await.unwrap() {
            CreateProfileOutcome::Created(profile) => profile,
            CreateProfileOutcome::Exists(_) => panic!("new profile unexpectedly existed"),
        };
        env_set(&db, &input.name, "TOKEN", "old").await.unwrap();
        remove(&db, &input.name).await.unwrap();
        let revived = upsert(&db, &input).await.unwrap();
        assert_eq!(revived.lifetime, created.lifetime + 1);
        assert!(env_meta(&db, &input.name).await.unwrap().is_empty());

        let mut target_input = source.as_input().unwrap();
        target_input.name = "revive-clone".to_string();
        let target = match create(&db, &target_input).await.unwrap() {
            CreateProfileOutcome::Created(profile) => profile,
            CreateProfileOutcome::Exists(_) => panic!("new clone target unexpectedly existed"),
        };
        env_set(&db, &target_input.name, "TOKEN", "old")
            .await
            .unwrap();
        remove(&db, &target_input.name).await.unwrap();
        let current_source = get(&db, DEFAULT_PROFILE).await.unwrap().unwrap();
        let cloned = create_clone(
            &db,
            DEFAULT_PROFILE,
            current_source.revision,
            &target_input,
            false,
        )
        .await
        .unwrap();
        let CloneProfileOutcome::Created(cloned) = cloned else {
            panic!("retired clone target was not revived")
        };
        assert_eq!(cloned.lifetime, target.lifetime + 1);
        assert!(env_meta(&db, &target_input.name).await.unwrap().is_empty());
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
