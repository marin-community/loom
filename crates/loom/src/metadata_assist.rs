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
use sqlx::FromRow;
use tokio::sync::{Mutex, Semaphore};
use weaver_api::{ResumptionCueView, ResumptionEvidenceView, TitleGenerationView};
use weaver_core::artifact;
use weaver_core::branch::{self, Branch, TitleProvenance, TitleUpdate, MAX_GENERATED_TITLE_CHARS};

use crate::agent::AgentManager;
use crate::history::{self, PageOptions};
use crate::profile::Profile;
use crate::session::Session;
use crate::{config, profile, repo_env, Db};

pub const METADATA_PROFILE_KEY: &str = "metadata.profile";
pub const TITLE_ENABLED_KEY: &str = "metadata.title_generation";
pub const CUES_ENABLED_KEY: &str = "metadata.resumption_cues";
pub const ALLOW_RESTRICTED_KEY: &str = "metadata.allow_restricted";
pub const CUE_INACTIVITY_KEY: &str = "metadata.resumption_inactivity_secs";

const PROMPT_TIMEOUT: Duration = Duration::from_secs(45);
const TITLE_GOAL_CHARS: usize = 4_000;
const CUE_SOURCE_CHARS: usize = 12_000;
const CUE_OUTPUT_CHARS: usize = 1_200;
const HISTORY_RECORDS: usize = 24;

static PROMPT_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(2));
static ACTIVE_CLAIMS: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));
// Prepare/commit and enable/disable transitions are short. Serializing them
// closes the only in-process race without inventing a job framework.
static STATE_GATE: Mutex<()> = Mutex::const_new(());

struct PromptClaim(String);

impl PromptClaim {
    fn acquire(key: String) -> Option<Self> {
        let inserted = ACTIVE_CLAIMS
            .lock()
            .expect("metadata claim mutex poisoned")
            .insert(key.clone());
        inserted.then_some(Self(key))
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
    artifacts: Vec<(String, i64)>,
}

struct CueSource {
    cursor: String,
    prompt: String,
    evidence: Vec<ResumptionEvidenceView>,
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
        } else {
            row.title_generation_status
        },
    })
}

pub async fn set_title_enabled(db: &Db, session_id: &str, enabled: bool) -> Result<()> {
    let _guard = STATE_GATE.lock().await;
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
    Ok(
        (!profile.retired && profile.protocol == "acp" && profile.is_automation_safe())
            .then_some(profile),
    )
}

fn privacy_allows_metadata(restricted: bool, allow_restricted: bool) -> bool {
    !restricted || allow_restricted
}

async fn known_secret_values(db: &Db, session: &Session, branch: &Branch) -> Vec<String> {
    let mut values = profile::env_pairs(db, &session.profile)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    values.extend(
        repo_env::pairs(db, &branch.repo_root)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(_, value)| value),
    );
    if let Ok(repo_config) = weaver_core::repo_config::load(std::path::Path::new(&branch.repo_root))
    {
        values.extend(repo_config.env.into_values());
    }
    values.retain(|value| value.chars().count() >= 4);
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
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

/// Start one best-effort title refresh. The model call is detached; all
/// eligibility and CAS checks are repeated at commit.
pub async fn spawn_title_generation(
    db: Db,
    session: Session,
    branch: Branch,
    explicit: bool,
) -> Result<bool> {
    let claim;
    {
        let _guard = STATE_GATE.lock().await;
        let state = row(&db, &session.id).await?;
        if !state.title_generation_enabled || !config::get_bool(&db, TITLE_ENABLED_KEY, true).await
        {
            mark_title_status(&db, &session.id, "disabled").await?;
            return Ok(false);
        }
        if !branch.title_provenance.can_generate(explicit) {
            mark_title_status(&db, &session.id, "protected").await?;
            return Ok(false);
        }
        if selected_profile(&db, &session).await?.is_none() {
            mark_title_status(&db, &session.id, "unavailable").await?;
            return Ok(false);
        }
        let Some(acquired) = PromptClaim::acquire(format!("title:{}", session.id)) else {
            return Ok(false);
        };
        claim = acquired;
        mark_title_status(&db, &session.id, "running").await?;
    }

    tokio::spawn(async move {
        let _claim = claim;
        if let Err(error) = generate_title(&db, &session, &branch).await {
            tracing::warn!(session = %session.id, %error, "metadata title generation failed");
            let _guard = STATE_GATE.lock().await;
            let _ = mark_title_status(&db, &session.id, "failed").await;
        }
    });
    Ok(true)
}

async fn generate_title(db: &Db, session: &Session, branch: &Branch) -> Result<()> {
    let Some(metadata_profile) = selected_profile(db, session).await? else {
        anyhow::bail!("metadata profile is unavailable");
    };
    let secrets = known_secret_values(db, session, branch).await;
    let prompt = title_prompt(branch, &secrets);
    let _permit = PROMPT_SLOTS.acquire().await?;
    let output = AgentManager::new(db)
        .run_oneshot(
            &metadata_profile.agent_kind,
            &prompt,
            "",
            "",
            Some(&metadata_profile),
            PROMPT_TIMEOUT,
        )
        .await
        .and_then(|text| branch::sanitize_generated_title(&text))
        .context("metadata agent returned no usable task label")?;
    drop(_permit);

    let _guard = STATE_GATE.lock().await;
    let state = row(db, &session.id).await?;
    if !state.title_generation_enabled {
        mark_title_status(db, &session.id, "disabled").await?;
        return Ok(());
    }
    match branch::replace_title(
        db,
        &branch.id,
        &branch.title,
        branch.title_provenance,
        &output,
        TitleProvenance::Generated,
    )
    .await?
    {
        TitleUpdate::Applied(_) => mark_title_status(db, &session.id, "generated").await?,
        TitleUpdate::Stale(_) => mark_title_status(db, &session.id, "protected").await?,
        TitleUpdate::Missing => mark_title_status(db, &session.id, "failed").await?,
    }
    Ok(())
}

fn sanitize_cue(input: &str) -> Option<String> {
    let plain: String = input
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect();
    let trimmed = plain.trim();
    (!trimmed.is_empty()).then(|| take_chars(trimmed, CUE_OUTPUT_CHARS))
}

async fn cue_source(db: &Db, session: &Session, branch: &Branch) -> Result<Option<CueSource>> {
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
    let Some(last) = page.records.last() else {
        return Ok(None);
    };
    let artifacts = artifact::list_for_session(db, &branch.repo_root, &branch.id).await?;
    let cursor = CueCursor {
        history: last.cursor.clone(),
        artifacts: artifacts
            .iter()
            .map(|artifact| (artifact.name.clone(), artifact.rev))
            .collect(),
    };
    let cursor = serde_json::to_string(&cursor)?;
    let mut transcript = String::new();
    for record in &page.records {
        let label = record
            .role
            .as_deref()
            .or(record.tool_name.as_deref())
            .or(record.event_name.as_deref())
            .unwrap_or(&record.kind);
        let content = record.content.as_deref().unwrap_or_default();
        transcript.push_str(&format!("[{}] {}: {}\n", record.cursor, label, content));
    }
    let artifact_lines = artifacts
        .iter()
        .map(|artifact| format!("- {} rev {}", artifact.name, artifact.rev))
        .collect::<Vec<_>>()
        .join("\n");
    let evidence = std::iter::once(ResumptionEvidenceView {
        kind: "conversation".to_string(),
        label: format!("Conversation through {}", last.cursor),
        href: format!("/s/{}?tab=conversation", session.id),
        cursor: last.cursor.clone(),
    })
    .chain(artifacts.iter().map(|artifact| ResumptionEvidenceView {
        kind: "artifact".to_string(),
        label: format!("{} rev {}", artifact.name, artifact.rev),
        href: format!("/s/{}/artifacts/{}", session.id, artifact.name),
        cursor: format!("artifact:{}:{}", artifact.name, artifact.rev),
    }))
    .collect();
    let secrets = known_secret_values(db, session, branch).await;
    let prompt = redact_known_secrets(
        format!(
            "Write a compact on-return cue for this work session. Cover current intent, \
             completed work, blockers, changed/current artifacts, and the likely next step. \
             Use only the evidence below; say when a category is unknown. Maximum 160 words. \
             Do not present the cue as authoritative state.\n\nGoal:\n{}\n\n\
             Recent source-linked conversation:\n{}\nArtifacts:\n{}",
            take_chars(&branch.goal, TITLE_GOAL_CHARS),
            take_chars(&transcript, CUE_SOURCE_CHARS),
            artifact_lines,
        ),
        &secrets,
    );
    Ok(Some(CueSource {
        cursor,
        prompt,
        evidence,
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

fn cue_cache_matches(state: &AssistanceRow, source_cursor: &str) -> bool {
    state.cue_source_cursor.as_deref() == Some(source_cursor) && state.cue_text.is_some()
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
    let Some(source) = cue_source(db, session, branch).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let state = row(db, &session.id).await?;
    if cue_cache_matches(&state, &source.cursor) {
        let evidence = serde_json::from_str(&state.cue_evidence).unwrap_or_default();
        return Ok(view(
            "generated",
            Some(source.cursor),
            state.cue_text,
            state.cue_generated_at,
            evidence,
        ));
    }
    if selected_profile(db, session).await?.is_none() {
        return Ok(view(
            "unavailable",
            Some(source.cursor),
            None,
            None,
            source.evidence,
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
        Some(source.cursor),
        None,
        None,
        source.evidence,
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
    let Some(source) = cue_source(db, session, branch).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let Some(metadata_profile) = selected_profile(db, session).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    let Some(_claim) = PromptClaim::acquire(format!("cue:{}:{}", session.id, source.cursor)) else {
        return Ok(view(
            "generating",
            Some(source.cursor),
            None,
            None,
            source.evidence,
        ));
    };
    let _permit = PROMPT_SLOTS.acquire().await?;
    let output = AgentManager::new(db)
        .run_oneshot(
            &metadata_profile.agent_kind,
            &source.prompt,
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
            Some(source.cursor),
            None,
            None,
            source.evidence,
        ));
    };
    // Conversation/artifact state may have advanced during the bounded model
    // call. Never label stale prose with a newer cursor.
    let Some(current_source) = cue_source(db, session, branch).await? else {
        return Ok(view("unavailable", None, None, None, Vec::new()));
    };
    if current_source.cursor != source.cursor {
        return Ok(view(
            "due",
            Some(current_source.cursor),
            None,
            None,
            current_source.evidence,
        ));
    }
    let generated_at = weaver_core::db::now_iso();
    let evidence = serde_json::to_string(&source.evidence)?;
    row(db, &session.id).await?;
    sqlx::query(
        "UPDATE session_metadata_assistance
         SET cue_source_cursor = ?, cue_text = ?, cue_generated_at = ?,
             cue_evidence = ?, updated_at = ?
         WHERE session_id = ?",
    )
    .bind(&source.cursor)
    .bind(&text)
    .bind(&generated_at)
    .bind(evidence)
    .bind(&generated_at)
    .bind(&session.id)
    .execute(db)
    .await?;
    Ok(view(
        "generated",
        Some(source.cursor),
        Some(text),
        Some(generated_at),
        source.evidence,
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
            artifacts: vec![("goal".into(), 1)],
        };
        assert_eq!(
            serde_json::to_string(&cursor).unwrap(),
            r#"{"history":"iris:1:0","artifacts":[["goal",1]]}"#
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
            artifacts: vec![("goal".into(), 2)],
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
    fn prompt_claims_are_keyed_single_flight_and_release_on_drop() {
        let key = "metadata-domain-test:cue:session:cursor".to_string();
        let first = PromptClaim::acquire(key.clone()).expect("first claimant");
        assert!(PromptClaim::acquire(key.clone()).is_none());
        assert!(PromptClaim::acquire(format!("{key}:other")).is_some());
        drop(first);
        assert!(PromptClaim::acquire(key).is_some());
    }
}
