//! Bounded metadata assistance for task labels and on-return conversation cues.
//!
//! This is deliberately one small service over the existing profile-aware
//! transient ACP prompt. It has no scheduler: launch may detach one title
//! refresh, while resumption cues run only through an explicit ensure request.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use tokio::sync::Semaphore;
use weaver_api::{HistoryPageView, ResumptionCueView, ResumptionEvidenceView, TitleGenerationView};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch::{self, Branch, TitleProvenance, TitleUpdate, MAX_GENERATED_TITLE_CHARS};
use weaver_core::events::EventBus;

use crate::agent::AgentManager;
use crate::history::{self, PageOptions};
use crate::profile::Profile;
use crate::session::Session;
use crate::{config, events, profile, repo_env, session, Db};

pub const METADATA_PROFILE_KEY: &str = "metadata.profile";
pub const TITLE_ENABLED_KEY: &str = "metadata.title_generation";
pub const CUES_ENABLED_KEY: &str = "metadata.resumption_cues";
pub const ALLOW_RESTRICTED_KEY: &str = "metadata.allow_restricted";
pub const CUE_INACTIVITY_KEY: &str = "metadata.resumption_inactivity_secs";

const PROMPT_TIMEOUT: Duration = Duration::from_secs(45);
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(10);
const TITLE_GOAL_CHARS: usize = 4_000;
const CUE_SOURCE_CHARS: usize = 12_000;
const CUE_OUTPUT_CHARS: usize = 1_200;
const HISTORY_RECORDS: usize = 24;
const CUE_ARTIFACTS: usize = 24;
const ARTIFACT_LABEL_CHARS: usize = 120;

static PROMPT_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(2));
static ACTIVE_CLAIMS: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

struct PromptClaim(String);

impl PromptClaim {
    fn acquire(key: String) -> Option<Self> {
        let inserted = ACTIVE_CLAIMS
            .lock()
            .expect("metadata claim mutex poisoned")
            .insert(key.clone());
        inserted.then_some(Self(key))
    }

    fn active(key: &str) -> bool {
        ACTIVE_CLAIMS
            .lock()
            .expect("metadata claim mutex poisoned")
            .contains(key)
    }
}

impl Drop for PromptClaim {
    fn drop(&mut self) {
        ACTIVE_CLAIMS
            .lock()
            .expect("metadata claim mutex poisoned")
            .remove(&self.0);
    }
}

#[derive(Debug, Clone, FromRow)]
struct AssistanceRow {
    title_generation_enabled: bool,
    title_generation_status: String,
    cue_source_cursor: Option<String>,
    cue_text: Option<String>,
    cue_generated_at: Option<String>,
    cue_evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CueCursor {
    history: String,
    fingerprint: String,
    artifacts: Vec<(i64, String, i64)>,
}

#[derive(Clone)]
struct CueIdentity {
    cursor: String,
    evidence: Vec<ResumptionEvidenceView>,
}

struct CueInputs {
    page: HistoryPageView,
    artifacts: Vec<Artifact>,
}

struct PreparedCue {
    identity: CueIdentity,
    prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileIdentity {
    name: String,
    lifetime: i64,
    revision: i64,
}

impl From<&Profile> for ProfileIdentity {
    fn from(profile: &Profile) -> Self {
        Self {
            name: profile.name.clone(),
            lifetime: profile.lifetime,
            revision: profile.revision,
        }
    }
}

#[derive(Debug, Clone)]
struct TitleFence {
    goal: String,
    title: String,
    provenance: TitleProvenance,
    restricted: bool,
    profile: ProfileIdentity,
}

struct CurrentTitleState<'a> {
    goal: &'a str,
    title: &'a str,
    provenance: TitleProvenance,
    restricted: bool,
    session_enabled: bool,
    globally_enabled: bool,
    allow_restricted: bool,
    profile: Option<&'a ProfileIdentity>,
}

async fn row(db: &Db, session_id: &str) -> Result<AssistanceRow> {
    sqlx::query(
        "INSERT INTO session_metadata_assistance
         (session_id, updated_at) VALUES (?, ?)
         ON CONFLICT(session_id) DO NOTHING",
    )
    .bind(session_id)
    .bind(weaver_core::db::now_iso())
    .execute(db)
    .await?;
    sqlx::query_as(
        "SELECT title_generation_enabled, title_generation_status,
                cue_source_cursor, cue_text, cue_generated_at, cue_evidence
         FROM session_metadata_assistance WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .context("reading session metadata assistance")
}

pub async fn title_view(
    db: &Db,
    session_id: &str,
    provenance: TitleProvenance,
) -> Result<TitleGenerationView> {
    let row = row(db, session_id).await?;
    let globally_enabled = config::get_bool(db, TITLE_ENABLED_KEY, true).await;
    Ok(TitleGenerationView {
        enabled: row.title_generation_enabled && globally_enabled,
        status: if !row.title_generation_enabled || !globally_enabled {
            "disabled".to_string()
        } else if !provenance.can_generate(true) {
            "protected".to_string()
        } else if PromptClaim::active(&title_claim_key(session_id)) {
            "running".to_string()
        } else {
            row.title_generation_status
        },
    })
}

pub async fn set_title_enabled(db: &Db, session_id: &str, enabled: bool) -> Result<()> {
    row(db, session_id).await?;
    sqlx::query(
        "UPDATE session_metadata_assistance
         SET title_generation_enabled = ?,
             title_generation_status = ?,
             updated_at = ?
         WHERE session_id = ?",
    )
    .bind(enabled)
    .bind(if enabled { "idle" } else { "disabled" })
    .bind(weaver_core::db::now_iso())
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn selected_profile(db: &Db, session: &Session) -> Result<Option<Profile>> {
    let name = config::get(db, METADATA_PROFILE_KEY)
        .await
        .unwrap_or_default();
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if !privacy_allows_metadata(
        session.policy_restricted,
        config::get_bool(db, ALLOW_RESTRICTED_KEY, false).await,
    ) {
        return Ok(None);
    }
    let Some(profile) = profile::get(db, name).await? else {
        return Ok(None);
    };
    Ok(metadata_profile_eligible(&profile).then_some(profile))
}

pub fn metadata_profile_eligible(profile: &Profile) -> bool {
    !profile.retired && profile.protocol == "acp" && profile.is_automation_safe()
}

fn privacy_allows_metadata(restricted: bool, allow_restricted: bool) -> bool {
    !restricted || allow_restricted
}

fn collect_secret_sources(
    sources: impl IntoIterator<Item = Result<Vec<String>>>,
) -> Result<Vec<String>> {
    let mut values = Vec::new();
    for source in sources {
        values.extend(source?);
    }
    values.retain(|value| value.chars().count() >= 4);
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    Ok(values)
}

async fn known_secret_values(
    db: &Db,
    session: &Session,
    branch: &Branch,
    metadata_profile: &Profile,
) -> Result<Vec<String>> {
    let source_profile = profile::env_pairs(db, &session.profile)
        .await
        .map(|pairs| pairs.into_iter().map(|(_, value)| value).collect());
    let metadata_profile = if metadata_profile.name == session.profile {
        Ok(Vec::new())
    } else {
        profile::env_pairs(db, &metadata_profile.name)
            .await
            .map(|pairs| pairs.into_iter().map(|(_, value)| value).collect())
    };
    let repo = repo_env::pairs(db, &branch.repo_root)
        .await
        .map(|pairs| pairs.into_iter().map(|(_, value)| value).collect());
    let repo_file = weaver_core::repo_config::load(std::path::Path::new(&branch.repo_root))
        .map(|config| config.env.into_values().collect());
    collect_secret_sources([source_profile, metadata_profile, repo, repo_file])
}

fn redact_known_secrets(mut text: String, secrets: &[String]) -> String {
    for secret in secrets {
        text = text.replace(secret, "<redacted>");
    }
    text
}

fn take_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn title_prompt(branch: &Branch, secrets: &[String]) -> String {
    redact_known_secrets(
        format!(
            "Return only a plain-text task label, at most {MAX_GENERATED_TITLE_CHARS} characters. \
             No quotes, markdown, prefix, explanation, group, or repository name.\n\n\
             Current fallback: {}\nGoal:\n{}",
            take_chars(&branch.title, MAX_GENERATED_TITLE_CHARS),
            take_chars(&branch.goal, TITLE_GOAL_CHARS),
        ),
        secrets,
    )
}

async fn mark_title_status(db: &Db, session_id: &str, status: &str) -> Result<()> {
    row(db, session_id).await?;
    sqlx::query(
        "UPDATE session_metadata_assistance
         SET title_generation_status = ?, updated_at = ? WHERE session_id = ?",
    )
    .bind(status)
    .bind(weaver_core::db::now_iso())
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

fn title_fence_status(fence: &TitleFence, current: &CurrentTitleState<'_>) -> Option<&'static str> {
    if !current.session_enabled || !current.globally_enabled {
        return Some("disabled");
    }
    if !current.provenance.can_generate(true) {
        return Some("protected");
    }
    if !privacy_allows_metadata(current.restricted, current.allow_restricted) {
        return Some("unavailable");
    }
    if current.profile != Some(&fence.profile) {
        return Some("unavailable");
    }
    if current.restricted != fence.restricted
        || current.goal != fence.goal
        || current.title != fence.title
        || current.provenance != fence.provenance
    {
        return Some("stale");
    }
    None
}

/// Start one best-effort title refresh. The model call is detached; all
/// eligibility and CAS checks are repeated at commit.
pub async fn spawn_title_generation(
    db: Db,
    bus: EventBus,
    session: Session,
    branch: Branch,
    explicit: bool,
) -> Result<()> {
    let state = row(&db, &session.id).await?;
    if !state.title_generation_enabled || !config::get_bool(&db, TITLE_ENABLED_KEY, true).await {
        mark_title_status(&db, &session.id, "disabled").await?;
        return Ok(());
    }
    if !branch.title_provenance.can_generate(explicit) {
        mark_title_status(&db, &session.id, "protected").await?;
        return Ok(());
    }
    let Some(metadata_profile) = selected_profile(&db, &session).await? else {
        mark_title_status(&db, &session.id, "unavailable").await?;
        return Ok(());
    };
    let Some(claim) = PromptClaim::acquire(title_claim_key(&session.id)) else {
        return Ok(());
    };
    let fence = TitleFence {
        goal: branch.goal.clone(),
        title: branch.title.clone(),
        provenance: branch.title_provenance,
        restricted: session.policy_restricted,
        profile: ProfileIdentity::from(&metadata_profile),
    };
    mark_title_status(&db, &session.id, "running").await?;

    tokio::spawn(async move {
        let _claim = claim;
        let status = match generate_title(&db, &session, &branch, &metadata_profile, &fence).await {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(session = %session.id, %error, "metadata title generation failed");
                "failed"
            }
        };
        if let Err(error) = mark_title_status(&db, &session.id, status).await {
            tracing::warn!(
                session = %session.id,
                %error,
                "failed to persist terminal metadata title status"
            );
        }
        events::emit(
            &bus,
            &branch.id,
            "metadata",
            serde_json::json!({
                "session_id": session.id,
                "title_generation": status,
            }),
        );
    });
    Ok(())
}

async fn generate_title(
    db: &Db,
    session: &Session,
    branch: &Branch,
    metadata_profile: &Profile,
    fence: &TitleFence,
) -> Result<&'static str> {
    let _permit = PROMPT_SLOTS.acquire().await?;
    let secrets = tokio::time::timeout(
        PREPARATION_TIMEOUT,
        known_secret_values(db, session, branch, metadata_profile),
    )
    .await
    .context("metadata title preparation timed out")??;
    let prompt = title_prompt(branch, &secrets);
    let output = AgentManager::new(db)
        .run_oneshot(
            &metadata_profile.agent_kind,
            &prompt,
            "",
            "",
            Some(metadata_profile),
            PROMPT_TIMEOUT,
        )
        .await
        .and_then(|text| branch::sanitize_generated_title(&text))
        .context("metadata agent returned no usable task label")?;
    drop(_permit);

    let Some(current_session) = session::get(db, &session.id).await? else {
        return Ok("failed");
    };
    let Some(current_branch) = branch::get(db, &branch.id).await? else {
        return Ok("failed");
    };
    let state = row(db, &session.id).await?;
    let globally_enabled = config::get_bool(db, TITLE_ENABLED_KEY, true).await;
    let allow_restricted = config::get_bool(db, ALLOW_RESTRICTED_KEY, false).await;
    let current_profile = selected_profile(db, &current_session).await?;
    let current_profile = current_profile.as_ref().map(ProfileIdentity::from);
    let current = CurrentTitleState {
        goal: &current_branch.goal,
        title: &current_branch.title,
        provenance: current_branch.title_provenance,
        restricted: current_session.policy_restricted,
        session_enabled: state.title_generation_enabled,
        globally_enabled,
        allow_restricted,
        profile: current_profile.as_ref(),
    };
    if let Some(status) = title_fence_status(fence, &current) {
        return Ok(status);
    }
    Ok(
        match branch::replace_title_from_goal(
            db,
            &branch.id,
            &fence.goal,
            &fence.title,
            fence.provenance,
            &output,
        )
        .await?
        {
            TitleUpdate::Applied(_) => "generated",
            TitleUpdate::Stale(current) if !current.title_provenance.can_generate(true) => {
                "protected"
            }
            TitleUpdate::Stale(_) => "stale",
            TitleUpdate::Missing => "failed",
        },
    )
}

fn sanitize_cue(input: &str) -> Option<String> {
    let plain: String = input
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect();
    let trimmed = plain.trim();
    (!trimmed.is_empty()).then(|| take_chars(trimmed, CUE_OUTPUT_CHARS))
}

fn recent_artifacts(mut artifacts: Vec<Artifact>) -> Vec<Artifact> {
    artifacts.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    artifacts.truncate(CUE_ARTIFACTS);
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));
    artifacts
}

async fn cue_inputs(db: &Db, session: &Session, branch: &Branch) -> Result<Option<CueInputs>> {
    let page = history::page(
        db,
        session,
        branch,
        PageOptions {
            limit: Some(HISTORY_RECORDS),
            ..Default::default()
        },
    )
    .await
    .map_err(|error| match error {
        history::PageError::BadRequest(message) => anyhow::anyhow!(message),
        history::PageError::Internal(error) => error,
    })?;
    if page.records.is_empty() {
        return Ok(None);
    }
    let artifacts =
        recent_artifacts(artifact::list_for_session(db, &branch.repo_root, &branch.id).await?);
    Ok(Some(CueInputs { page, artifacts }))
}

fn source_fingerprint<T: Serialize>(
    source: &str,
    records: &T,
    artifacts: &[(i64, String, i64)],
) -> Result<String> {
    let encoded = serde_json::to_vec(&(source, records, artifacts))?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn cue_identity_from(session: &Session, inputs: &CueInputs) -> Result<CueIdentity> {
    let last = inputs
        .page
        .records
        .last()
        .expect("cue inputs require at least one history record");
    let artifact_versions = inputs
        .artifacts
        .iter()
        .map(|artifact| (artifact.id, artifact.name.clone(), artifact.rev))
        .collect::<Vec<_>>();
    let cursor = CueCursor {
        history: last.cursor.clone(),
        fingerprint: source_fingerprint(
            &inputs.page.source,
            &inputs.page.records,
            &artifact_versions,
        )?,
        artifacts: artifact_versions,
    };
    let cursor = serde_json::to_string(&cursor)?;
    let evidence = std::iter::once(ResumptionEvidenceView {
        kind: "conversation".to_string(),
        label: format!("Conversation through {}", last.cursor),
        href: format!("/s/{}?tab=conversation", session.id),
        cursor: last.cursor.clone(),
    })
    .chain(inputs.artifacts.iter().map(|artifact| {
        let name = percent_encoding::utf8_percent_encode(
            &artifact.name,
            percent_encoding::NON_ALPHANUMERIC,
        );
        ResumptionEvidenceView {
            kind: "artifact".to_string(),
            label: format!(
                "{} rev {}",
                take_chars(&artifact.name, ARTIFACT_LABEL_CHARS),
                artifact.rev
            ),
            href: format!("/s/{}/artifacts/{name}", session.id),
            cursor: format!("artifact:{}:{}", artifact.id, artifact.rev),
        }
    }))
    .collect();
    Ok(CueIdentity { cursor, evidence })
}

async fn cue_identity(db: &Db, session: &Session, branch: &Branch) -> Result<Option<CueIdentity>> {
    Ok(match cue_inputs(db, session, branch).await? {
        Some(inputs) => Some(cue_identity_from(session, &inputs)?),
        None => None,
    })
}

fn take_tail_chars(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    if count <= limit {
        return value.to_string();
    }
    let tail: String = value.chars().skip(count - (limit - 1)).collect();
    format!("…{tail}")
}

fn recent_transcript(page: &HistoryPageView) -> String {
    let mut transcript = String::new();
    for record in &page.records {
        let label = record
            .role
            .as_deref()
            .or(record.tool_name.as_deref())
            .or(record.event_name.as_deref())
            .unwrap_or(&record.kind);
        let tool_input = record
            .tool_input
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok())
            .unwrap_or_default();
        let content = record
            .content
            .as_deref()
            .or(record.tool_status.as_deref())
            .unwrap_or(&tool_input);
        transcript.push_str(&format!("[{}] {}: {}\n", record.cursor, label, content));
    }
    take_tail_chars(&transcript, CUE_SOURCE_CHARS)
}

async fn prepare_cue(
    db: &Db,
    session: &Session,
    branch: &Branch,
    metadata_profile: &Profile,
) -> Result<Option<PreparedCue>> {
    let Some(inputs) = cue_inputs(db, session, branch).await? else {
        return Ok(None);
    };
    let identity = cue_identity_from(session, &inputs)?;
    let artifact_lines = inputs
        .artifacts
        .iter()
        .map(|artifact| {
            format!(
                "- {} rev {}: {}",
                take_chars(&artifact.name, ARTIFACT_LABEL_CHARS),
                artifact.rev,
                take_chars(&artifact.title, ARTIFACT_LABEL_CHARS)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let secrets = known_secret_values(db, session, branch, metadata_profile).await?;
    let prompt = redact_known_secrets(
        format!(
            "Write a compact on-return cue for this work session. Cover current intent, \
             completed work, blockers, changed/current artifacts, and the likely next step. \
             Use only the evidence below; say when a category is unknown. Maximum 160 words. \
             Do not present the cue as authoritative state.\n\nGoal:\n{}\n\n\
             Recent source-linked conversation:\n{}\nArtifacts:\n{}",
            take_chars(&branch.goal, TITLE_GOAL_CHARS),
            recent_transcript(&inputs.page),
            artifact_lines,
        ),
        &secrets,
    );
    Ok(Some(PreparedCue { identity, prompt }))
}

fn inactivity_elapsed(last_activity_at: Option<&str>, threshold_secs: i64) -> bool {
    let Some(last) = last_activity_at else {
        return false;
    };
    DateTime::parse_from_rfc3339(last)
        .map(|last| Utc::now().signed_duration_since(last).num_seconds() >= threshold_secs)
        .unwrap_or(false)
}

fn cue_cache_matches(state: &AssistanceRow, source_cursor: &str) -> bool {
    state.cue_source_cursor.as_deref() == Some(source_cursor) && state.cue_text.is_some()
}

fn cue_claim_key(session_id: &str) -> String {
    format!("cue:{session_id}")
}

fn title_claim_key(session_id: &str) -> String {
    format!("title:{session_id}")
}

fn view(
    status: &str,
    source_cursor: Option<String>,
    text: Option<String>,
    generated_at: Option<String>,
    evidence: Vec<ResumptionEvidenceView>,
) -> ResumptionCueView {
    ResumptionCueView {
        status: status.to_string(),
        source_cursor,
        text,
        generated_at,
        evidence,
    }
}

/// Read the current cache and whether a bounded explicit ensure is due. This
/// never invokes an agent.
pub async fn current_cue(db: &Db, session: &Session, branch: &Branch) -> Result<ResumptionCueView> {
    if !config::get_bool(db, CUES_ENABLED_KEY, true).await {
        return Ok(view("disabled", None, None, None, Vec::new()));
    }
    let Some(identity) = cue_identity(db, session, branch).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let state = row(db, &session.id).await?;
    if cue_cache_matches(&state, &identity.cursor) {
        let evidence = serde_json::from_str(&state.cue_evidence).unwrap_or_default();
        return Ok(view(
            "generated",
            Some(identity.cursor),
            state.cue_text,
            state.cue_generated_at,
            evidence,
        ));
    }
    if PromptClaim::active(&cue_claim_key(&session.id)) {
        return Ok(view(
            "generating",
            Some(identity.cursor),
            None,
            None,
            identity.evidence,
        ));
    }
    if selected_profile(db, session).await?.is_none() {
        return Ok(view(
            "unavailable",
            Some(identity.cursor),
            None,
            None,
            identity.evidence,
        ));
    }
    let threshold = config::get(db, CUE_INACTIVITY_KEY)
        .await
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3_600)
        .max(0);
    Ok(view(
        if inactivity_elapsed(session.last_activity_at.as_deref(), threshold) {
            "due"
        } else {
            "not_due"
        },
        Some(identity.cursor),
        None,
        None,
        identity.evidence,
    ))
}

/// Generate or reuse one cue for the current source cursor. `force` is the
/// explicit user action; an on-return caller leaves it false and respects the
/// configured inactivity threshold.
pub async fn ensure_cue(
    db: &Db,
    session: &Session,
    branch: &Branch,
    force: bool,
) -> Result<ResumptionCueView> {
    let current = current_cue(db, session, branch).await?;
    if (!force && current.status != "due")
        || matches!(current.status.as_str(), "disabled" | "unavailable")
    {
        return Ok(current);
    }
    let claim_key = cue_claim_key(&session.id);
    let Some(_claim) = PromptClaim::acquire(claim_key) else {
        return Ok(view(
            "generating",
            current.source_cursor,
            None,
            None,
            current.evidence,
        ));
    };
    let Some(metadata_profile) = selected_profile(db, session).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let profile_identity = ProfileIdentity::from(&metadata_profile);
    let _permit = PROMPT_SLOTS.acquire().await?;
    let Some(prepared) = tokio::time::timeout(
        PREPARATION_TIMEOUT,
        prepare_cue(db, session, branch, &metadata_profile),
    )
    .await
    .context("resumption cue preparation timed out")??
    else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let output = AgentManager::new(db)
        .run_oneshot(
            &metadata_profile.agent_kind,
            &prepared.prompt,
            "",
            "",
            Some(&metadata_profile),
            PROMPT_TIMEOUT,
        )
        .await
        .and_then(|text| sanitize_cue(&text));
    drop(_permit);
    let Some(text) = output else {
        return Ok(view(
            "unavailable",
            Some(prepared.identity.cursor),
            None,
            None,
            prepared.identity.evidence,
        ));
    };
    // Conversation/artifact state may have advanced during the bounded model
    // call. Recompute the content fingerprint and never label stale prose with
    // a newer cursor.
    let Some(current_identity) = cue_identity(db, session, branch).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    if current_identity.cursor != prepared.identity.cursor {
        return Ok(view(
            "due",
            Some(current_identity.cursor),
            None,
            None,
            current_identity.evidence,
        ));
    }
    if !config::get_bool(db, CUES_ENABLED_KEY, true).await {
        return Ok(view("disabled", None, None, None, Vec::new()));
    }
    if selected_profile(db, session)
        .await?
        .as_ref()
        .map(ProfileIdentity::from)
        .as_ref()
        != Some(&profile_identity)
    {
        return Ok(view(
            "unavailable",
            Some(prepared.identity.cursor),
            None,
            None,
            prepared.identity.evidence,
        ));
    }
    let generated_at = weaver_core::db::now_iso();
    let evidence = serde_json::to_string(&prepared.identity.evidence)?;
    row(db, &session.id).await?;
    sqlx::query(
        "UPDATE session_metadata_assistance
         SET cue_source_cursor = ?, cue_text = ?, cue_generated_at = ?,
             cue_evidence = ?, updated_at = ?
         WHERE session_id = ?",
    )
    .bind(&prepared.identity.cursor)
    .bind(&text)
    .bind(&generated_at)
    .bind(evidence)
    .bind(&generated_at)
    .bind(&session.id)
    .execute(db)
    .await?;
    Ok(view(
        "generated",
        Some(prepared.identity.cursor),
        Some(text),
        Some(generated_at),
        prepared.identity.evidence,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_and_bounds_are_deterministic() {
        assert_eq!(
            redact_known_secrets(
                "token=abcdefgh and abcdefghij".to_string(),
                &["abcdefghij".to_string(), "abcdefgh".to_string()],
            ),
            "token=<redacted> and <redacted>"
        );
        assert_eq!(take_chars("abc", 3), "abc");
        assert_eq!(take_chars("abcd", 3), "abc…");
        assert_eq!(take_tail_chars("abcdef", 4), "…def");
        assert_eq!(
            sanitize_cue("\0  Continue\nnext  "),
            Some("Continue\nnext".into())
        );
    }

    #[test]
    fn inactivity_is_bounded_by_the_configured_threshold() {
        let recent = Utc::now().to_rfc3339();
        assert!(!inactivity_elapsed(Some(&recent), 60));
        let old = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        assert!(inactivity_elapsed(Some(&old), 60));
        assert!(!inactivity_elapsed(None, 0));
    }

    #[test]
    fn cursor_and_cache_identity_include_artifact_revisions() {
        let cursor = CueCursor {
            history: "iris:1:0".into(),
            fingerprint: "abc123".into(),
            artifacts: vec![(42, "goal".into(), 1)],
        };
        assert_eq!(
            serde_json::to_string(&cursor).unwrap(),
            r#"{"history":"iris:1:0","fingerprint":"abc123","artifacts":[[42,"goal",1]]}"#
        );
        let state = AssistanceRow {
            title_generation_enabled: true,
            title_generation_status: "idle".into(),
            cue_source_cursor: Some(serde_json::to_string(&cursor).unwrap()),
            cue_text: Some("Continue the focused work.".into()),
            cue_generated_at: Some("2026-07-26T00:00:00Z".into()),
            cue_evidence: "[]".into(),
        };
        let current = state.cue_source_cursor.as_deref().unwrap();
        assert!(cue_cache_matches(&state, current));
        let advanced = CueCursor {
            artifacts: vec![(42, "goal".into(), 2)],
            ..cursor
        };
        assert!(!cue_cache_matches(
            &state,
            &serde_json::to_string(&advanced).unwrap()
        ));
    }

    #[test]
    fn restricted_sessions_require_an_explicit_metadata_opt_in() {
        assert!(privacy_allows_metadata(false, false));
        assert!(!privacy_allows_metadata(true, false));
        assert!(privacy_allows_metadata(true, true));
    }

    #[test]
    fn source_fingerprint_covers_content_and_immutable_artifact_identity() {
        let artifacts = vec![(42, "goal".to_string(), 1)];
        let before = source_fingerprint(
            "acp",
            &serde_json::json!([{"cursor":"acp:1:2","text":"old"}]),
            &artifacts,
        )
        .unwrap();
        let updated = source_fingerprint(
            "acp",
            &serde_json::json!([{"cursor":"acp:1:2","text":"new"}]),
            &artifacts,
        )
        .unwrap();
        let recreated = source_fingerprint(
            "acp",
            &serde_json::json!([{"cursor":"acp:1:2","text":"old"}]),
            &[(99, "goal".to_string(), 1)],
        )
        .unwrap();
        assert_ne!(before, updated, "in-place ACP updates change identity");
        assert_ne!(
            before, recreated,
            "delete/recreate changes immutable artifact identity"
        );
    }

    #[test]
    fn title_fence_rejects_every_source_and_eligibility_change() {
        let profile = ProfileIdentity {
            name: "watch".into(),
            lifetime: 1,
            revision: 7,
        };
        let fence = TitleFence {
            goal: "ship it".into(),
            title: "ship it".into(),
            provenance: TitleProvenance::Derived,
            restricted: false,
            profile: profile.clone(),
        };
        let status = |goal,
                      title,
                      provenance,
                      restricted,
                      session_enabled,
                      global_enabled,
                      allow_restricted,
                      profile: Option<&ProfileIdentity>| {
            let current = CurrentTitleState {
                goal,
                title,
                provenance,
                restricted,
                session_enabled,
                globally_enabled: global_enabled,
                allow_restricted,
                profile,
            };
            title_fence_status(&fence, &current)
        };
        assert_eq!(
            status(
                "ship it",
                "ship it",
                TitleProvenance::Derived,
                false,
                true,
                true,
                false,
                Some(&profile)
            ),
            None
        );
        assert_eq!(
            status(
                "changed",
                "ship it",
                TitleProvenance::Derived,
                false,
                true,
                true,
                false,
                Some(&profile)
            ),
            Some("stale")
        );
        assert_eq!(
            status(
                "ship it",
                "human",
                TitleProvenance::User,
                false,
                true,
                true,
                false,
                Some(&profile)
            ),
            Some("protected")
        );
        assert_eq!(
            status(
                "ship it",
                "ship it",
                TitleProvenance::Derived,
                false,
                false,
                true,
                false,
                Some(&profile)
            ),
            Some("disabled")
        );
        assert_eq!(
            status(
                "ship it",
                "ship it",
                TitleProvenance::Derived,
                false,
                true,
                false,
                false,
                Some(&profile)
            ),
            Some("disabled")
        );
        assert_eq!(
            status(
                "ship it",
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
                true,
                false,
                Some(&profile)
            ),
            Some("unavailable")
        );
        assert_eq!(
            status(
                "ship it",
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
                true,
                true,
                Some(&profile)
            ),
            Some("stale"),
            "a changed restricted-session policy invalidates the source fence"
        );
        let changed_profile = ProfileIdentity {
            revision: 8,
            ..profile.clone()
        };
        assert_eq!(
            status(
                "ship it",
                "ship it",
                TitleProvenance::Derived,
                false,
                true,
                true,
                false,
                Some(&changed_profile)
            ),
            Some("unavailable")
        );
    }

    #[test]
    fn secret_resolution_fails_closed_on_any_source_error() {
        let result = collect_secret_sources([
            Ok(vec!["first-secret".into()]),
            Err(anyhow::anyhow!("secret backend unavailable")),
            Ok(vec!["later-secret".into()]),
        ]);
        assert_eq!(
            result.unwrap_err().to_string(),
            "secret backend unavailable"
        );
    }

    #[test]
    fn prompt_claims_are_per_session_single_flight_and_release_on_drop() {
        let key = cue_claim_key("metadata-domain-test-session");
        let first = PromptClaim::acquire(key.clone()).expect("first claimant");
        assert!(PromptClaim::acquire(key.clone()).is_none());
        assert!(PromptClaim::acquire(cue_claim_key("another-session")).is_some());
        drop(first);
        assert!(PromptClaim::acquire(key).is_some());
    }
}
