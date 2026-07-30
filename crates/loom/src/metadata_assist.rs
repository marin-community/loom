//! Bounded metadata assistance for task labels and on-return conversation cues.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::{Duration, UNIX_EPOCH};

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
use crate::{config, events, profile, repo_env, session, user_token, Db};

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
    source: String,
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
    fence: CueFence,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct MetadataSource {
    goal: String,
    restricted: bool,
    allow_restricted: bool,
    session_profile: ProfileIdentity,
    source_profile: Option<ProfileIdentity>,
    created_by: Option<String>,
    creator_credential: (bool, Option<String>),
    repo_env_generation: Vec<(String, String)>,
    repo_config_generation: Option<(u64, u64)>,
    metadata_agent: String,
}

struct MetadataRead {
    session: Session,
    branch: Branch,
    source: MetadataSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CueFence {
    source: MetadataSource,
    cursor: String,
    prompt_fingerprint: String,
}

#[derive(Debug, Clone)]
struct TitleFence {
    source: MetadataSource,
    title: String,
    provenance: TitleProvenance,
}

struct CurrentTitleState<'a> {
    source: Option<&'a MetadataSource>,
    title: &'a str,
    provenance: TitleProvenance,
    session_enabled: bool,
    globally_enabled: bool,
}

type TitlePreflight = std::result::Result<MetadataRead, &'static str>;
type CueBoundaryCheck = std::result::Result<MetadataRead, ResumptionCueView>;

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

fn privacy_allows_metadata(restricted: bool, allow_restricted: bool) -> bool {
    !restricted || allow_restricted
}

fn serialized_fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

async fn launching_user_token(db: &Db, created_by: Option<&str>) -> Result<Option<String>> {
    let Some(username) = created_by else {
        return Ok(None);
    };
    Ok(user_token::get(db, username)
        .await?
        .filter(|value| !value.trim().is_empty()))
}

async fn metadata_source(
    db: &Db,
    session: &Session,
    branch: &Branch,
    allow_restricted: bool,
) -> Result<MetadataSource> {
    let source_profile = profile::get(db, &session.profile)
        .await?
        .as_ref()
        .map(ProfileIdentity::from);
    let creator_credential = match session.created_by.as_deref() {
        Some(username) => {
            let status = user_token::status(db, username).await?;
            (status.set, status.updated_at)
        }
        None => (false, None),
    };
    let repo_env_generation = repo_env::list(db, &branch.repo_root)
        .await?
        .into_iter()
        .map(|entry| (entry.name, entry.updated_at))
        .collect();
    let repo_config_path =
        std::path::Path::new(&branch.repo_root).join(weaver_core::repo_config::CONFIG_REL_PATH);
    let repo_config_generation = match std::fs::metadata(repo_config_path) {
        Ok(metadata) => Some((
            metadata.len(),
            metadata
                .modified()?
                .duration_since(UNIX_EPOCH)?
                .as_nanos()
                .try_into()
                .context("repo config modified time exceeds u64")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(MetadataSource {
        goal: branch.goal.clone(),
        restricted: session.policy_restricted,
        allow_restricted,
        session_profile: ProfileIdentity {
            name: session.profile.clone(),
            lifetime: session.profile_lifetime,
            revision: session.profile_revision,
        },
        source_profile,
        created_by: session.created_by.clone(),
        creator_credential,
        repo_env_generation,
        repo_config_generation,
        metadata_agent: session.agent_kind.clone(),
    })
}

async fn metadata_read(db: &Db, session_id: &str, branch_id: &str) -> Result<Option<MetadataRead>> {
    let Some(session) = session::get(db, session_id).await? else {
        return Ok(None);
    };
    let Some(branch) = branch::get(db, branch_id).await? else {
        return Ok(None);
    };
    let allow_restricted = config::get_bool(db, ALLOW_RESTRICTED_KEY, false).await;
    let source = metadata_source(db, &session, &branch, allow_restricted).await?;
    Ok(Some(MetadataRead {
        session,
        branch,
        source,
    }))
}

fn metadata_read_is_eligible(read: &MetadataRead) -> bool {
    privacy_allows_metadata(read.source.restricted, read.source.allow_restricted)
        && read.session.protocol == "acp"
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

async fn known_secret_values(db: &Db, session: &Session, branch: &Branch) -> Result<Vec<String>> {
    let source_profile = profile::env_pairs(db, &session.profile)
        .await
        .map(|pairs| pairs.into_iter().map(|(_, value)| value).collect());
    let repo = repo_env::pairs(db, &branch.repo_root)
        .await
        .map(|pairs| pairs.into_iter().map(|(_, value)| value).collect());
    let repo_file = weaver_core::repo_config::load(std::path::Path::new(&branch.repo_root))
        .map(|config| config.env.into_values().collect());
    let creator_token = launching_user_token(db, session.created_by.as_deref())
        .await
        .map(|value| value.into_iter().collect());
    collect_secret_sources([source_profile, repo, repo_file, creator_token])
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
    let Some(source) = current.source else {
        return Some("unavailable");
    };
    if !privacy_allows_metadata(source.restricted, source.allow_restricted) {
        return Some("unavailable");
    }
    if source.metadata_agent != fence.source.metadata_agent {
        return Some("unavailable");
    }
    if source != &fence.source
        || current.title != fence.title
        || current.provenance != fence.provenance
    {
        return Some("stale");
    }
    None
}

async fn title_preflight(
    db: &Db,
    session_id: &str,
    branch_id: &str,
    fence: &TitleFence,
) -> Result<TitlePreflight> {
    let state = row(db, session_id).await?;
    let globally_enabled = config::get_bool(db, TITLE_ENABLED_KEY, true).await;
    let Some(read) = metadata_read(db, session_id, branch_id).await? else {
        return Ok(Err("failed"));
    };
    let current_title = read.branch.title.clone();
    let current_provenance = read.branch.title_provenance;
    let current = CurrentTitleState {
        source: metadata_read_is_eligible(&read).then_some(&read.source),
        title: &current_title,
        provenance: current_provenance,
        session_enabled: state.title_generation_enabled,
        globally_enabled,
    };
    if let Some(status) = title_fence_status(fence, &current) {
        return Ok(Err(status));
    }
    Ok(Ok(read))
}

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
    let Some(snapshot) = metadata_read(&db, &session.id, &branch.id).await? else {
        mark_title_status(&db, &session.id, "unavailable").await?;
        return Ok(());
    };
    if !snapshot.branch.title_provenance.can_generate(explicit) {
        mark_title_status(&db, &snapshot.session.id, "protected").await?;
        return Ok(());
    }
    if !metadata_read_is_eligible(&snapshot) {
        mark_title_status(&db, &session.id, "unavailable").await?;
        return Ok(());
    }
    let MetadataRead {
        session,
        branch,
        source,
        ..
    } = snapshot;
    let Some(claim) = PromptClaim::acquire(title_claim_key(&session.id)) else {
        return Ok(());
    };
    let fence = TitleFence {
        source,
        title: branch.title.clone(),
        provenance: branch.title_provenance,
    };
    mark_title_status(&db, &session.id, "running").await?;

    tokio::spawn(async move {
        let _claim = claim;
        let status = match generate_title(&db, &session, &branch, &fence).await {
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
    fence: &TitleFence,
) -> Result<&'static str> {
    let _permit = PROMPT_SLOTS.acquire().await?;
    let prepared = match title_preflight(db, &session.id, &branch.id, fence).await? {
        Ok(snapshot) => snapshot,
        Err(status) => return Ok(status),
    };
    let secrets = tokio::time::timeout(
        PREPARATION_TIMEOUT,
        known_secret_values(db, &prepared.session, &prepared.branch),
    )
    .await
    .context("metadata title preparation timed out")??;
    let prompt = title_prompt(&prepared.branch, &secrets);
    let run = match title_preflight(db, &session.id, &branch.id, fence).await? {
        Ok(snapshot) => snapshot,
        Err(status) => return Ok(status),
    };
    let output = AgentManager::new(db)
        .run_metadata(&run.session.agent_kind, &prompt, PROMPT_TIMEOUT)
        .await
        .and_then(|text| branch::sanitize_generated_title(&text))
        .context("metadata agent returned no usable task label")?;
    drop(_permit);

    let _current = match title_preflight(db, &session.id, &branch.id, fence).await? {
        Ok(snapshot) => snapshot,
        Err(status) => return Ok(status),
    };
    Ok(
        match branch::replace_title_from_goal(
            db,
            &branch.id,
            &fence.source.goal,
            &fence.title,
            fence.provenance,
            &output,
        )
        .await?
        {
            TitleUpdate::Applied(_) => {
                crate::channels::update_branch_channel_names(db, &branch.id, &output).await?;
                "generated"
            }
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

fn cue_identity_from(
    session: &Session,
    inputs: &CueInputs,
    source: &MetadataSource,
) -> Result<CueIdentity> {
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
        source: serialized_fingerprint(source)?,
        history: last.cursor.clone(),
        fingerprint: serialized_fingerprint(&(
            &inputs.page.source,
            &inputs.page.records,
            &artifact_versions,
        ))?,
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

async fn cue_identity(
    db: &Db,
    session: &Session,
    branch: &Branch,
    source: &MetadataSource,
) -> Result<Option<CueIdentity>> {
    Ok(match cue_inputs(db, session, branch).await? {
        Some(inputs) => Some(cue_identity_from(session, &inputs, source)?),
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

async fn prepare_cue(db: &Db, snapshot: &MetadataRead) -> Result<Option<PreparedCue>> {
    let Some(inputs) = cue_inputs(db, &snapshot.session, &snapshot.branch).await? else {
        return Ok(None);
    };
    let identity = cue_identity_from(&snapshot.session, &inputs, &snapshot.source)?;
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
    let secrets = known_secret_values(db, &snapshot.session, &snapshot.branch).await?;
    let prompt = redact_known_secrets(
        format!(
            "Write a compact on-return cue for this work session. Cover current intent, \
             completed work, blockers, changed/current artifacts, and the likely next step. \
             Use only the evidence below; say when a category is unknown. Maximum 160 words. \
             Do not present the cue as authoritative state.\n\nGoal:\n{}\n\n\
             Recent source-linked conversation:\n{}\nArtifacts:\n{}",
            take_chars(&snapshot.branch.goal, TITLE_GOAL_CHARS),
            recent_transcript(&inputs.page),
            artifact_lines,
        ),
        &secrets,
    );
    let fence = CueFence {
        source: snapshot.source.clone(),
        cursor: identity.cursor.clone(),
        prompt_fingerprint: serialized_fingerprint(&prompt)?,
    };
    Ok(Some(PreparedCue {
        identity,
        prompt,
        fence,
    }))
}

fn inactivity_elapsed(last_activity_at: Option<&str>, threshold_secs: i64) -> bool {
    let Some(last) = last_activity_at else {
        return false;
    };
    DateTime::parse_from_rfc3339(last)
        .map(|last| Utc::now().signed_duration_since(last).num_seconds() >= threshold_secs)
        .unwrap_or(false)
}

async fn cue_inactivity_threshold(db: &Db) -> i64 {
    config::get(db, CUE_INACTIVITY_KEY)
        .await
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3_600)
        .max(0)
}

async fn cue_is_due(db: &Db, session: &Session) -> bool {
    inactivity_elapsed(
        session.last_activity_at.as_deref(),
        cue_inactivity_threshold(db).await,
    )
}

async fn bounded_prepare_cue(db: &Db, snapshot: &MetadataRead) -> Result<Option<PreparedCue>> {
    tokio::time::timeout(PREPARATION_TIMEOUT, prepare_cue(db, snapshot))
        .await
        .context("resumption cue preparation timed out")?
}

fn cue_fence_status(expected: &CueFence, current: &CueFence) -> Option<&'static str> {
    cue_boundary_status(expected, &current.source, &current.cursor)
        .or_else(|| (current.prompt_fingerprint != expected.prompt_fingerprint).then_some("due"))
}

fn cue_boundary_status(
    expected: &CueFence,
    source: &MetadataSource,
    cursor: &str,
) -> Option<&'static str> {
    if source.metadata_agent != expected.source.metadata_agent {
        Some("unavailable")
    } else if source != &expected.source || cursor != expected.cursor {
        Some("due")
    } else {
        None
    }
}

fn prepared_view(status: &str, prepared: &PreparedCue) -> ResumptionCueView {
    identity_view(status, &prepared.identity)
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

fn identity_view(status: &str, identity: &CueIdentity) -> ResumptionCueView {
    view(
        status,
        Some(identity.cursor.clone()),
        None,
        None,
        identity.evidence.clone(),
    )
}

async fn checked_cue_boundary(
    db: &Db,
    session_id: &str,
    branch_id: &str,
    force: bool,
    expected: &PreparedCue,
) -> Result<CueBoundaryCheck> {
    let enabled = config::get_bool(db, CUES_ENABLED_KEY, true).await;
    let threshold = cue_inactivity_threshold(db).await;
    let snapshot = match metadata_read(db, session_id, branch_id).await? {
        Some(snapshot) if metadata_read_is_eligible(&snapshot) => snapshot,
        _ => return Ok(Err(prepared_view("unavailable", expected))),
    };
    let Some(identity) =
        cue_identity(db, &snapshot.session, &snapshot.branch, &snapshot.source).await?
    else {
        return Ok(Err(prepared_view("unavailable", expected)));
    };
    if !enabled {
        return Ok(Err(view("disabled", None, None, None, Vec::new())));
    }
    if !force && !inactivity_elapsed(snapshot.session.last_activity_at.as_deref(), threshold) {
        return Ok(Err(identity_view("not_due", &identity)));
    }
    if let Some(status) = cue_boundary_status(&expected.fence, &snapshot.source, &identity.cursor) {
        return Ok(Err(identity_view(status, &identity)));
    }
    Ok(Ok(snapshot))
}

pub async fn current_cue(db: &Db, session: &Session, branch: &Branch) -> Result<ResumptionCueView> {
    if !config::get_bool(db, CUES_ENABLED_KEY, true).await {
        return Ok(view("disabled", None, None, None, Vec::new()));
    }
    let Some(read) = metadata_read(db, &session.id, &branch.id).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let Some(identity) = cue_identity(db, &read.session, &read.branch, &read.source).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let state = row(db, &read.session.id).await?;
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
    if PromptClaim::active(&cue_claim_key(&read.session.id)) {
        return Ok(identity_view("generating", &identity));
    }
    if !metadata_read_is_eligible(&read) {
        return Ok(identity_view("unavailable", &identity));
    }
    Ok(identity_view(
        if cue_is_due(db, &read.session).await {
            "due"
        } else {
            "not_due"
        },
        &identity,
    ))
}

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
    let _permit = PROMPT_SLOTS.acquire().await?;
    if !config::get_bool(db, CUES_ENABLED_KEY, true).await {
        return Ok(view("disabled", None, None, None, Vec::new()));
    }
    let Some(snapshot) = metadata_read(db, &session.id, &branch.id).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    if !metadata_read_is_eligible(&snapshot) {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let Some(prepared) = bounded_prepare_cue(db, &snapshot).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };

    // This boundary is the final await before the model call.
    let run_snapshot =
        match checked_cue_boundary(db, &session.id, &branch.id, force, &prepared).await? {
            Ok(snapshot) => snapshot,
            Err(view) => return Ok(view),
        };
    let output = AgentManager::new(db)
        .run_metadata(
            &run_snapshot.session.agent_kind,
            &prepared.prompt,
            PROMPT_TIMEOUT,
        )
        .await
        .and_then(|text| sanitize_cue(&text));
    let Some(text) = output else {
        return Ok(prepared_view("unavailable", &prepared));
    };

    // Recompute every prompt source after the model returns.
    let Some(current_snapshot) = metadata_read(db, &session.id, &branch.id).await? else {
        return Ok(prepared_view("unavailable", &prepared));
    };
    if !metadata_read_is_eligible(&current_snapshot) {
        return Ok(prepared_view("unavailable", &prepared));
    };
    let Some(current_prepared) = bounded_prepare_cue(db, &current_snapshot).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    if let Some(status) = cue_fence_status(&prepared.fence, &current_prepared.fence) {
        return Ok(prepared_view(status, &current_prepared));
    }
    row(db, &session.id).await?;
    if let Err(view) =
        checked_cue_boundary(db, &session.id, &branch.id, force, &current_prepared).await?
    {
        return Ok(view);
    }
    let generated_at = weaver_core::db::now_iso();
    let evidence = serde_json::to_string(&current_prepared.identity.evidence)?;
    sqlx::query(
        "UPDATE session_metadata_assistance
         SET cue_source_cursor = ?, cue_text = ?, cue_generated_at = ?,
             cue_evidence = ?, updated_at = ?
         WHERE session_id = ?",
    )
    .bind(&current_prepared.identity.cursor)
    .bind(&text)
    .bind(&generated_at)
    .bind(evidence)
    .bind(&generated_at)
    .bind(&session.id)
    .execute(db)
    .await?;
    Ok(view(
        "generated",
        Some(current_prepared.identity.cursor),
        Some(text),
        Some(generated_at),
        current_prepared.identity.evidence,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> MetadataSource {
        MetadataSource {
            goal: "ship it".into(),
            restricted: false,
            allow_restricted: false,
            session_profile: ProfileIdentity {
                name: "default".into(),
                lifetime: 1,
                revision: 4,
            },
            source_profile: Some(ProfileIdentity {
                name: "default".into(),
                lifetime: 1,
                revision: 9,
            }),
            created_by: Some("alice".into()),
            creator_credential: (true, Some("2026-07-26T00:00:00Z".into())),
            repo_env_generation: vec![("REGISTRY_TOKEN".into(), "2026-07-26T00:00:00Z".into())],
            repo_config_generation: Some((128, 1_722_000_000_000_000_000)),
            metadata_agent: "codex".into(),
        }
    }

    fn source_fingerprint<T: Serialize>(
        source: &str,
        records: &T,
        artifacts: &[(i64, String, i64)],
    ) -> Result<String> {
        serialized_fingerprint(&(source, records, artifacts))
    }

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
            source: "source-v1".into(),
            history: "iris:1:0".into(),
            fingerprint: "abc123".into(),
            artifacts: vec![(42, "goal".into(), 1)],
        };
        assert_eq!(
            serde_json::to_string(&cursor).unwrap(),
            r#"{"source":"source-v1","history":"iris:1:0","fingerprint":"abc123","artifacts":[[42,"goal",1]]}"#
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
    fn durable_source_fingerprint_covers_semantic_prompt_inputs() {
        let source = source();
        let baseline = serialized_fingerprint(&source).unwrap();
        let changed_sources = [
            MetadataSource {
                goal: "changed goal".into(),
                ..source.clone()
            },
            MetadataSource {
                restricted: true,
                allow_restricted: true,
                ..source.clone()
            },
            MetadataSource {
                allow_restricted: true,
                ..source.clone()
            },
            MetadataSource {
                session_profile: ProfileIdentity {
                    revision: 5,
                    ..source.session_profile.clone()
                },
                ..source.clone()
            },
            MetadataSource {
                source_profile: Some(ProfileIdentity {
                    revision: 10,
                    ..source.source_profile.clone().unwrap()
                }),
                ..source.clone()
            },
            MetadataSource {
                created_by: Some("bob".into()),
                ..source.clone()
            },
            MetadataSource {
                creator_credential: (true, Some("2026-07-26T00:01:00Z".into())),
                ..source.clone()
            },
            MetadataSource {
                repo_env_generation: vec![("REGISTRY_TOKEN".into(), "2026-07-26T00:01:00Z".into())],
                ..source.clone()
            },
            MetadataSource {
                repo_config_generation: Some((129, 1_722_000_000_000_000_001)),
                ..source.clone()
            },
            MetadataSource {
                metadata_agent: "claude".into(),
                ..source.clone()
            },
        ];
        for changed in changed_sources {
            assert_ne!(baseline, serialized_fingerprint(&changed).unwrap());
        }
    }

    #[test]
    fn title_fence_rejects_every_source_and_eligibility_change() {
        let source = source();
        let fence = TitleFence {
            source: source.clone(),
            title: "ship it".into(),
            provenance: TitleProvenance::Derived,
        };
        let status = |source: Option<&MetadataSource>,
                      title,
                      provenance,
                      session_enabled,
                      global_enabled| {
            let current = CurrentTitleState {
                source,
                title,
                provenance,
                session_enabled,
                globally_enabled: global_enabled,
            };
            title_fence_status(&fence, &current)
        };
        assert_eq!(
            status(
                Some(&source),
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
            ),
            None
        );
        let changed_goal = MetadataSource {
            goal: "changed".into(),
            ..source.clone()
        };
        assert_eq!(
            status(
                Some(&changed_goal),
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
            ),
            Some("stale")
        );
        assert_eq!(
            status(Some(&source), "human", TitleProvenance::User, true, true,),
            Some("protected")
        );
        assert_eq!(
            status(
                Some(&source),
                "ship it",
                TitleProvenance::Derived,
                false,
                true,
            ),
            Some("disabled")
        );
        assert_eq!(
            status(
                Some(&source),
                "ship it",
                TitleProvenance::Derived,
                true,
                false,
            ),
            Some("disabled")
        );
        assert_eq!(
            status(None, "ship it", TitleProvenance::Derived, true, true,),
            Some("unavailable")
        );
        let revoked = MetadataSource {
            restricted: true,
            allow_restricted: false,
            ..source.clone()
        };
        assert_eq!(
            status(
                Some(&revoked),
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
            ),
            Some("unavailable")
        );
        let changed_session_profile = MetadataSource {
            session_profile: ProfileIdentity {
                revision: 5,
                ..source.session_profile.clone()
            },
            ..source.clone()
        };
        assert_eq!(
            status(
                Some(&changed_session_profile),
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
            ),
            Some("stale"),
            "a changed source-session profile invalidates the prompt fence"
        );
        let changed_agent = MetadataSource {
            metadata_agent: "claude".into(),
            ..source.clone()
        };
        assert_eq!(
            status(
                Some(&changed_agent),
                "ship it",
                TitleProvenance::Derived,
                true,
                true,
            ),
            Some("unavailable")
        );
    }

    #[test]
    fn cue_fence_and_post_preparation_boundary_cover_every_prompt_source() {
        let source = source();
        let fence = CueFence {
            source: source.clone(),
            cursor: "cursor-v1".into(),
            prompt_fingerprint: "prompt-v1".into(),
        };
        assert_eq!(cue_fence_status(&fence, &fence), None);
        for changed in [
            CueFence {
                source: MetadataSource {
                    goal: "changed goal".into(),
                    ..source.clone()
                },
                ..fence.clone()
            },
            CueFence {
                source: MetadataSource {
                    creator_credential: (true, Some("2026-07-26T00:01:00Z".into())),
                    ..source.clone()
                },
                ..fence.clone()
            },
            CueFence {
                cursor: "cursor-v2".into(),
                ..fence.clone()
            },
            CueFence {
                prompt_fingerprint: "prompt-v2".into(),
                ..fence.clone()
            },
        ] {
            assert_eq!(cue_fence_status(&fence, &changed), Some("due"));
        }
        let changed_agent = CueFence {
            source: MetadataSource {
                metadata_agent: "claude".into(),
                ..source
            },
            ..fence.clone()
        };
        assert_eq!(
            cue_fence_status(&fence, &changed_agent),
            Some("unavailable")
        );
        assert_eq!(
            cue_boundary_status(&fence, &fence.source, &fence.cursor),
            None
        );
        let changed_goal = MetadataSource {
            goal: "changed after preparation".into(),
            ..fence.source.clone()
        };
        assert_eq!(
            cue_boundary_status(&fence, &changed_goal, &fence.cursor),
            Some("due")
        );
        assert_eq!(
            cue_boundary_status(&fence, &fence.source, "cursor-v2"),
            Some("due")
        );
        assert_eq!(
            cue_boundary_status(&fence, &changed_agent.source, &fence.cursor),
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

    #[tokio::test]
    async fn launching_user_token_is_a_fail_closed_secret_source() {
        let db = crate::db::connect_in_memory().await.unwrap();
        sqlx::query("INSERT INTO users (username) VALUES ('alice')")
            .execute(&db)
            .await
            .unwrap();
        user_token::set(&db, "alice", "github_pat_secret")
            .await
            .unwrap();
        let status = user_token::status(&db, "alice").await.unwrap();
        assert!(status.set);
        assert!(status.updated_at.is_some());
        assert!(!serde_json::to_string(&status)
            .unwrap()
            .contains("github_pat_secret"));
        assert_eq!(
            launching_user_token(&db, Some("alice")).await.unwrap(),
            Some("github_pat_secret".into())
        );
        user_token::set(&db, "alice", "").await.unwrap();
        assert_eq!(
            launching_user_token(&db, Some("alice")).await.unwrap(),
            None
        );
        sqlx::query("DROP TABLE user_github_tokens")
            .execute(&db)
            .await
            .unwrap();
        assert!(launching_user_token(&db, Some("alice")).await.is_err());
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
