//! Incremental and full catalogue reconciliation.
//!
//! Rows are written as each model's fetch lands, so an interrupted Cursor
//! pass keeps its progress and the next boot's incremental scope finishes it.
//! A failed probe leaves whatever rows already exist (stale-if-present).

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::probe::ProbeSession;
use super::store::{self, CatalogRow};
use crate::db::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Incremental,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Boot,
    /// A retry of a harness that has never stored a row. Does not start
    /// another retry loop.
    Bootstrap,
    Tick,
    Manual,
    Credential,
}

const DEFAULT_REFRESH_DAYS: i64 = 7;
const BOOTSTRAP_INITIAL: Duration = Duration::from_secs(2);
const BOOTSTRAP_CAP: Duration = Duration::from_secs(15 * 60);

pub async fn refresh_days(db: &Db) -> i64 {
    if let Ok(raw) = std::env::var("LOOM_CATALOG_REFRESH_DAYS") {
        if let Ok(days) = raw.trim().parse::<i64>() {
            return days.max(0);
        }
    }
    weaver_core::config::get(db, "catalog.refresh_days")
        .await
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_REFRESH_DAYS)
}

pub fn scope_for(rows: &[CatalogRow], version: &str, fingerprint: &str, stale: bool) -> Scope {
    let fetched: Vec<_> = rows
        .iter()
        .filter(|row| row.options_json.is_some())
        .collect();
    if rows.is_empty() || stale {
        return Scope::Full;
    }
    if fetched.is_empty() {
        return Scope::Incremental;
    }
    if fetched
        .iter()
        .any(|row| version_forces_full(&row.adapter_version, version))
    {
        return Scope::Full;
    }
    if fetched
        .iter()
        .any(|row| row.credential_fingerprint != fingerprint)
    {
        return Scope::Full;
    }
    Scope::Incremental
}

/// A blank or write-through sentinel is not an adapter upgrade. Only a stored
/// handshake version that differs from the one just observed forces a full refetch.
fn version_forces_full(stored: &str, handshake: &str) -> bool {
    !stored.is_empty() && stored != "live" && stored != handshake
}

pub fn rows_are_stale(rows: &[CatalogRow], days: i64, now: DateTime<Utc>) -> bool {
    if days == 0 {
        return rows.iter().any(|row| row.fetched_at.is_some());
    }
    let max_age = chrono::Duration::days(days);
    rows.iter()
        .filter_map(|row| row.fetched_at.as_deref())
        .any(|stamp| {
            DateTime::parse_from_rfc3339(stamp)
                .or_else(|_| DateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S%.fZ"))
                .ok()
                .is_some_and(|then| now.signed_duration_since(then) > max_age)
        })
}

/// Reconcile one harness against an already-open probe session.
pub async fn reconcile_open<S: ProbeSession>(
    db: &Db,
    harness: &str,
    mut scope: Scope,
    fingerprint: &str,
    session: &mut S,
) -> Result<()> {
    let handshake = session.handshake().clone();
    if handshake.version.is_empty() {
        anyhow::bail!("catalogue probe returned no adapter version");
    }
    let existing = store::list(db, harness).await?;
    if scope == Scope::Incremental
        && existing.iter().any(|row| {
            row.options_json.is_some()
                && version_forces_full(&row.adapter_version, &handshake.version)
        })
    {
        scope = Scope::Full;
    }

    let mut keep = Vec::new();
    for model in &handshake.models {
        store::note_listed(db, harness, &model.id, &model.label).await?;
        keep.push(model.id.clone());
    }
    store::prune(db, harness, &keep).await?;

    if let Some(default_model) = handshake.default_model.clone() {
        if !handshake.default_options.is_empty() {
            let label = handshake
                .models
                .iter()
                .find(|model| model.id == default_model)
                .map(|model| model.label.clone())
                .unwrap_or_else(|| default_model.clone());
            store::write_options(
                db,
                harness,
                &default_model,
                &label,
                &handshake.default_options,
                &handshake.version,
            )
            .await?;
        }
    }

    let pending = models_to_fetch(&handshake, &existing, scope);
    for model_id in pending {
        match session.fetch_model(&model_id).await {
            Ok(options) if !options.is_empty() => {
                let label = handshake
                    .models
                    .iter()
                    .find(|model| model.id == model_id)
                    .map(|model| model.label.clone())
                    .unwrap_or_else(|| model_id.clone());
                store::write_options(db, harness, &model_id, &label, &options, &handshake.version)
                    .await?;
            }
            Ok(_) => {
                tracing::warn!(
                    harness,
                    model = %model_id,
                    "model option fetch returned no configOptions"
                );
            }
            Err(error) => {
                tracing::warn!(harness, model = %model_id, %error, "model option fetch failed");
            }
        }
    }
    store::stamp_fingerprint(db, harness, fingerprint).await?;
    // Incremental passes can see a partial advertised list. Only a full pass
    // rewrites saved profile models onto a newer id in the same family.
    if scope == Scope::Full {
        super::succession::upgrade_profiles(db, harness, &keep).await?;
    }
    Ok(())
}

fn models_to_fetch(
    handshake: &super::probe::Handshake,
    existing: &[CatalogRow],
    scope: Scope,
) -> Vec<String> {
    handshake
        .models
        .iter()
        .filter(|model| handshake.default_model.as_deref() != Some(model.id.as_str()))
        .filter(|model| match scope {
            Scope::Full => true,
            Scope::Incremental => !existing
                .iter()
                .any(|row| row.model_id == model.id && row.options_json.is_some()),
        })
        .map(|model| model.id.clone())
        .collect()
}

pub async fn credential_fingerprint(db: &Db, harness: &str) -> String {
    let keys: &[&str] = match harness {
        "cursor-agent" => &["CURSOR_API_KEY", "CURSOR_AUTH_TOKEN"],
        "opencode" => &["OPENCODE_AUTH_JSON", "OPENCODE_CONFIG_JSON"],
        "claude" => &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        "codex" => &["OPENAI_API_KEY"],
        _ => &[],
    };
    let stored = crate::agent_env::pairs(db).await.unwrap_or_default();
    let mut material = String::new();
    for key in keys {
        let from_profile = stored
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone());
        let value = from_profile
            .or_else(|| std::env::var(key).ok())
            .unwrap_or_else(|| file_secret(harness, key));
        material.push_str(key);
        material.push('=');
        material.push_str(&hex_sha256(value.as_bytes()));
        material.push(';');
    }
    material
}

fn file_secret(harness: &str, key: &str) -> String {
    if harness != "opencode" {
        return String::new();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return String::new();
    };
    let path = match key {
        "OPENCODE_AUTH_JSON" => {
            std::path::PathBuf::from(home).join(".local/share/opencode/auth.json")
        }
        "OPENCODE_CONFIG_JSON" => {
            std::path::PathBuf::from(home).join(".config/opencode/opencode.json")
        }
        _ => return String::new(),
    };
    std::fs::read_to_string(path).unwrap_or_default()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// Background catalogue maintenance. Never blocks serving or launch: the
/// caller spawns this and returns.
pub async fn run(db: Db) {
    if disabled() {
        return;
    }
    if let Err(error) = pass(&db, Trigger::Boot).await {
        tracing::warn!(%error, "catalogue boot reconcile failed");
    }
    loop {
        tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
        if let Err(error) = pass(&db, Trigger::Tick).await {
            tracing::warn!(%error, "catalogue refresh tick failed");
        }
    }
}

/// `LOOM_CATALOG_DISABLED` turns off every catalogue probe. The integration
/// suite sets it so a test server never reaches for the host's real harnesses.
fn disabled() -> bool {
    std::env::var_os("LOOM_CATALOG_DISABLED").is_some_and(|value| !value.is_empty())
}

/// Queue a full catalogue refresh. Returns whether a refresh was started; it
/// is skipped when the catalogue is disabled.
pub fn spawn_refresh(db: Db, harness: Option<String>) -> bool {
    if disabled() {
        return false;
    }
    weaver_core::spawn_boxed(Box::pin(async move {
        let targets = match harness {
            Some(kind) => vec![kind],
            None => installed_harnesses().await,
        };
        for kind in targets {
            if let Err(error) =
                reconcile_harness(&db, &kind, Some(Scope::Full), Trigger::Manual).await
            {
                tracing::warn!(harness = %kind, %error, "manual catalogue refresh failed");
            }
        }
    }));
    true
}

async fn pass(db: &Db, trigger: Trigger) -> Result<()> {
    for harness in installed_harnesses().await {
        if let Err(error) = reconcile_harness(db, &harness, None, trigger).await {
            tracing::warn!(harness = %harness, %error, "catalogue reconcile failed");
        }
    }
    Ok(())
}

async fn installed_harnesses() -> Vec<String> {
    let mut kinds = Vec::new();
    for kind in crate::agent_kind::BuiltinAgentKind::ALL {
        if crate::agent::builtin_available_kind(kind).await {
            kinds.push(kind.as_str().to_string());
        }
    }
    kinds
}

pub async fn reconcile_harness(
    db: &Db,
    harness: &str,
    forced: Option<Scope>,
    trigger: Trigger,
) -> Result<()> {
    let Some(command) = crate::agent::acp_command_for(db, harness).await else {
        return Ok(());
    };
    let fingerprint = credential_fingerprint(db, harness).await;
    let rows = store::list(db, harness).await?;
    let days = refresh_days(db).await;
    let stale = rows_are_stale(&rows, days, Utc::now());
    let had_rows = !rows.is_empty();
    let preliminary = forced.unwrap_or_else(|| {
        // Version is known only after the handshake; a credential or age
        // mismatch is enough to force a full pass before the session opens.
        let credentials_changed = rows
            .iter()
            .any(|row| row.options_json.is_some() && row.credential_fingerprint != fingerprint);
        if !had_rows
            || stale
            || credentials_changed
            || trigger == Trigger::Credential
            || trigger == Trigger::Manual
        {
            Scope::Full
        } else {
            Scope::Incremental
        }
    });

    let env = probe_env(db).await;
    let opened = super::probe::LiveSession::open(&command, harness, &env).await;
    let mut session = match opened {
        Ok(session) => session,
        Err(error) => {
            if !had_rows && trigger == Trigger::Boot {
                spawn_bootstrap(db.clone(), harness.to_string());
            }
            return Err(error).context("opening catalogue probe");
        }
    };
    // The handshake version can still upgrade an incremental pass to full.
    let version = session.handshake().version.clone();
    let scope = match preliminary {
        Scope::Incremental => scope_for(&rows, &version, &fingerprint, false),
        Scope::Full => Scope::Full,
    };
    reconcile_open(db, harness, scope, &fingerprint, &mut session).await?;
    Ok(())
}

fn spawn_bootstrap(db: Db, harness: String) {
    weaver_core::spawn_boxed(Box::pin(async move {
        let mut delay = BOOTSTRAP_INITIAL;
        loop {
            tokio::time::sleep(delay).await;
            if store::has_rows(&db, &harness).await.unwrap_or(false) {
                return;
            }
            match reconcile_harness(&db, &harness, Some(Scope::Full), Trigger::Bootstrap).await {
                Ok(()) => {
                    if store::has_rows(&db, &harness).await.unwrap_or(false) {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(harness = %harness, %error, "catalogue bootstrap retry failed");
                }
            }
            delay = (delay * 2).min(BOOTSTRAP_CAP);
        }
    }));
}

async fn probe_env(db: &Db) -> Vec<(String, String)> {
    crate::agent_env::pairs(db).await.unwrap_or_default()
}

/// Launch write-through: the live session already holds this model's options.
///
/// Only refreshes a catalogue a probe has already established. A launch on a
/// harness nobody has probed would otherwise make one live model look like the
/// whole catalogue, which retires the static picker and stops bootstrap.
pub async fn write_through(db: &Db, harness: &str, model_id: &str, options: &[Value]) {
    if harness.is_empty() || model_id.is_empty() || options.is_empty() {
        return;
    }
    let rows = store::list(db, harness).await.unwrap_or_default();
    if rows.is_empty() {
        return;
    }
    let existing = rows.iter().find(|row| row.model_id == model_id);
    let label = existing
        .map(|row| row.label.clone())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| model_id.to_string());
    // Keep the handshake version already stored. Stamping a sentinel here
    // makes the next boot treat every row as an adapter upgrade and refetch.
    let version = existing
        .map(|row| row.adapter_version.as_str())
        .filter(|version| !version.is_empty() && *version != "live")
        .or_else(|| {
            rows.iter()
                .map(|row| row.adapter_version.as_str())
                .find(|version| !version.is_empty() && *version != "live")
        })
        .unwrap_or("");
    if let Err(error) = store::write_options(db, harness, model_id, &label, options, version).await
    {
        tracing::warn!(harness, model = %model_id, %error, "catalogue write-through failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::probe::{Handshake, ListedModel, ScriptedSession};
    use serde_json::json;
    use std::collections::HashMap;

    fn fetched(version: &str) -> store::CatalogRow {
        store::CatalogRow {
            model_id: "alpha".into(),
            label: "alpha".into(),
            options_json: Some("[]".into()),
            adapter_version: version.into(),
            credential_fingerprint: "fp".into(),
            fetched_at: Some("2026-01-01T00:00:00Z".into()),
        }
    }

    #[test]
    fn a_write_through_sentinel_stays_on_the_incremental_path() {
        assert_eq!(
            scope_for(&[fetched("live")], "1.2.3", "fp", false),
            Scope::Incremental
        );
        assert_eq!(
            scope_for(&[fetched("1.2.3")], "1.2.3", "fp", false),
            Scope::Incremental
        );
        assert_eq!(
            scope_for(&[fetched("1.0.0")], "1.2.3", "fp", false),
            Scope::Full
        );
    }

    fn model(id: &str) -> ListedModel {
        ListedModel {
            id: id.to_string(),
            label: id.to_string(),
        }
    }

    fn handshake(default_options: Value) -> Handshake {
        Handshake {
            version: "1".into(),
            models: vec![model("alpha"), model("beta"), model("gamma")],
            default_model: Some("alpha".into()),
            default_options: default_options.as_array().cloned().unwrap(),
            model_config_id: "model".into(),
        }
    }

    fn options_for(model_id: &str) -> Vec<Value> {
        vec![json!({
            "id": "model",
            "category": "model",
            "currentValue": model_id,
            "options": [{"value": model_id, "name": model_id}]
        })]
    }

    #[tokio::test]
    async fn full_reconcile_writes_each_model_and_prunes() {
        let db = crate::db::connect_in_memory().await.unwrap();
        store::note_listed(&db, "claude", "gone", "Gone")
            .await
            .unwrap();
        let mut session = ScriptedSession {
            handshake: handshake(json!([{
                "id": "model", "category": "model", "currentValue": "alpha"
            }])),
            fetched: HashMap::from([
                ("beta".into(), Ok(options_for("beta"))),
                ("gamma".into(), Ok(options_for("gamma"))),
            ]),
        };
        reconcile_open(&db, "claude", Scope::Full, "fp", &mut session)
            .await
            .unwrap();
        let rows = store::list(&db, "claude").await.unwrap();
        let ids: Vec<_> = rows.iter().map(|row| row.model_id.as_str()).collect();
        assert_eq!(ids, ["alpha", "beta", "gamma"]);
        assert!(rows.iter().all(|row| row.options_json.is_some()));
        assert!(rows.iter().all(|row| row.credential_fingerprint == "fp"));
    }

    #[tokio::test]
    async fn partial_failure_keeps_progress_and_incremental_resumes() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut first = ScriptedSession {
            handshake: handshake(json!([{"id":"model","category":"model","currentValue":"alpha"}])),
            fetched: HashMap::from([
                ("beta".into(), Ok(options_for("beta"))),
                ("gamma".into(), Err("internal error".into())),
            ]),
        };
        reconcile_open(&db, "cursor-agent", Scope::Full, "fp", &mut first)
            .await
            .unwrap();
        let rows = store::list(&db, "cursor-agent").await.unwrap();
        assert!(rows
            .iter()
            .any(|row| row.model_id == "gamma" && row.options_json.is_none()));
        assert!(rows
            .iter()
            .any(|row| row.model_id == "beta" && row.options_json.is_some()));

        let mut second = ScriptedSession {
            handshake: handshake(json!([{"id":"model","category":"model","currentValue":"alpha"}])),
            fetched: HashMap::from([("gamma".into(), Ok(options_for("gamma")))]),
        };
        reconcile_open(&db, "cursor-agent", Scope::Incremental, "fp", &mut second)
            .await
            .unwrap();
        let rows = store::list(&db, "cursor-agent").await.unwrap();
        assert!(rows.iter().all(|row| row.options_json.is_some()));
    }

    #[test]
    fn staleness_is_per_row_age() {
        let fresh = CatalogRow {
            model_id: "a".into(),
            label: "A".into(),
            options_json: Some("[]".into()),
            adapter_version: "1".into(),
            credential_fingerprint: "fp".into(),
            fetched_at: Some(Utc::now().to_rfc3339()),
        };
        let now = Utc::now();
        assert!(!rows_are_stale(std::slice::from_ref(&fresh), 7, now));
        let old = CatalogRow {
            fetched_at: Some("2020-01-01T00:00:00.000Z".into()),
            ..fresh
        };
        assert!(rows_are_stale(&[old], 7, now));
    }

    #[tokio::test]
    async fn a_full_refresh_moves_a_dropped_profile_model_to_its_family() {
        let db = crate::db::connect_in_memory().await.unwrap();
        sqlx::query(
            "UPDATE profiles SET agent_kind = 'cursor-agent', model = 'claude-opus-4-8', revision = 3 WHERE name = 'default'",
        )
        .execute(&db)
        .await
        .unwrap();
        let mut session = ScriptedSession {
            handshake: Handshake {
                version: "1".into(),
                models: vec![model("claude-opus-5-5"), model("claude-sonnet-5-5")],
                default_model: Some("claude-opus-5-5".into()),
                default_options: options_for("claude-opus-5-5"),
                model_config_id: "model".into(),
            },
            fetched: HashMap::from([(
                "claude-sonnet-5-5".into(),
                Ok(options_for("claude-sonnet-5-5")),
            )]),
        };
        reconcile_open(&db, "cursor-agent", Scope::Full, "fp", &mut session)
            .await
            .unwrap();
        let (model, revision): (String, i64) =
            sqlx::query_as("SELECT model, revision FROM profiles WHERE name = 'default'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(model, "claude-opus-5-5");
        assert_eq!(revision, 4);
    }

    #[tokio::test]
    async fn an_incremental_pass_does_not_rewrite_a_saved_model() {
        let db = crate::db::connect_in_memory().await.unwrap();
        sqlx::query(
            "UPDATE profiles SET agent_kind = 'cursor-agent', model = 'claude-opus-4-8', revision = 3 WHERE name = 'default'",
        )
        .execute(&db)
        .await
        .unwrap();
        let mut session = ScriptedSession {
            handshake: Handshake {
                version: "1".into(),
                models: vec![model("claude-opus-5-5"), model("claude-sonnet-5-5")],
                default_model: Some("claude-opus-5-5".into()),
                default_options: options_for("claude-opus-5-5"),
                model_config_id: "model".into(),
            },
            fetched: HashMap::new(),
        };
        reconcile_open(&db, "cursor-agent", Scope::Incremental, "fp", &mut session)
            .await
            .unwrap();
        let (model, revision): (String, i64) =
            sqlx::query_as("SELECT model, revision FROM profiles WHERE name = 'default'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(model, "claude-opus-4-8");
        assert_eq!(revision, 3);
    }

    #[tokio::test]
    async fn an_empty_option_fetch_keeps_the_stored_options() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut first = ScriptedSession {
            handshake: handshake(json!([{
                "id": "model", "category": "model", "currentValue": "alpha"
            }])),
            fetched: HashMap::from([
                ("beta".into(), Ok(options_for("beta"))),
                ("gamma".into(), Ok(options_for("gamma"))),
            ]),
        };
        reconcile_open(&db, "claude", Scope::Full, "fp", &mut first)
            .await
            .unwrap();
        let before = store::list(&db, "claude").await.unwrap();
        let beta_before = before
            .iter()
            .find(|row| row.model_id == "beta")
            .unwrap()
            .options_json
            .clone();

        let mut second = ScriptedSession {
            handshake: handshake(json!([{
                "id": "model", "category": "model", "currentValue": "alpha"
            }])),
            fetched: HashMap::from([("beta".into(), Ok(vec![])), ("gamma".into(), Ok(vec![]))]),
        };
        reconcile_open(&db, "claude", Scope::Full, "fp", &mut second)
            .await
            .unwrap();
        let rows = store::list(&db, "claude").await.unwrap();
        let beta = rows.iter().find(|row| row.model_id == "beta").unwrap();
        assert_eq!(beta.options_json, beta_before);
    }
}
