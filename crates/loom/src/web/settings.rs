use axum::{extract::State, Extension, Json};
use serde_json::{json, Value};
use weaver_api::{SettingsEnvelope, UserPreferenceView, UserPreferencesEnvelope};

use crate::auth::{self, Principal};
use crate::config;
use crate::db::Db;
use crate::profile;

use super::{ApiResult, AppError, AppState};

const PERSONAL_PREFERENCE_KEYS: &[&str] =
    &["terminal.theme", "terminal.font", "terminal.font_size"];

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async fn settings_envelope(db: &Db) -> ApiResult<Json<SettingsEnvelope>> {
    let settings = config::describe(db)
        .await?
        .into_iter()
        .map(|setting| setting.into())
        .collect();
    Ok(Json(SettingsEnvelope { settings }))
}

pub(super) async fn get_settings(State(st): State<AppState>) -> ApiResult<Json<SettingsEnvelope>> {
    settings_envelope(&st.db).await
}

pub(super) async fn patch_settings(
    State(st): State<AppState>,
    Json(body): Json<serde_json::Map<String, Value>>,
) -> ApiResult<Json<SettingsEnvelope>> {
    let mut changes: Vec<config::Change> = Vec::with_capacity(body.len());
    let mut legacy_agent_changes: Vec<config::Change> = Vec::new();
    let mut errors = serde_json::Map::new();

    for (key, raw) in body {
        let legacy_agent = matches!(
            key.as_str(),
            "agent.default" | "agent.model" | "agent.effort" | "agent.mode"
        );
        if config::spec(&key).is_none() && !legacy_agent {
            errors.insert(key, json!("unknown setting"));
            continue;
        }
        let value = match raw {
            Value::Null => None,
            Value::String(s) => Some(s),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => {
                errors.insert(
                    key,
                    json!("value must be a string, number, boolean, or null"),
                );
                continue;
            }
        };
        if !legacy_agent {
            if let Some(value) = &value {
                if let Err(why) = config::validate(&key, value) {
                    errors.insert(key, json!(why));
                    continue;
                }
            }
        }
        if legacy_agent {
            legacy_agent_changes.push((key, value));
        } else {
            changes.push((key, value));
        }
    }

    if !errors.is_empty() {
        let message = if errors.len() == 1 {
            let (key, why) = errors.iter().next().unwrap();
            format!("{key}: {}", why.as_str().unwrap_or("invalid"))
        } else {
            "one or more settings are invalid".to_string()
        };
        return Err(AppError::bad_request(message).with_details(Value::Object(errors)));
    }
    let _profile_permit = if legacy_agent_changes.is_empty() {
        None
    } else {
        Some(
            st.launch_gate
                .acquire_profile(profile::DEFAULT_PROFILE)
                .await,
        )
    };
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    if !legacy_agent_changes.is_empty() {
        apply_legacy_agent_patch(&st.db, &legacy_agent_changes)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?;
    }
    config::apply(&st.db, &changes).await?;
    let keys: Vec<&str> = changes
        .iter()
        .chain(&legacy_agent_changes)
        .map(|(k, _)| k.as_str())
        .collect();
    tracing::info!(keys = ?keys, "settings updated");
    settings_envelope(&st.db).await
}

async fn preferences_envelope(db: &Db, username: &str) -> ApiResult<Json<UserPreferencesEnvelope>> {
    let overrides = auth::user_preferences(db, username).await?;
    let preferences = config::describe(db)
        .await?
        .into_iter()
        .filter(|setting| PERSONAL_PREFERENCE_KEYS.contains(&setting.spec.key))
        .map(|setting| {
            let inherited_value = setting.value;
            let override_value = overrides.get(setting.spec.key);
            UserPreferenceView {
                key: setting.spec.key.to_string(),
                label: setting.spec.label.to_string(),
                description: setting.spec.description.to_string(),
                kind: setting.spec.kind.into(),
                options: setting
                    .spec
                    .options
                    .iter()
                    .map(|option| (*option).to_string())
                    .collect(),
                value: override_value
                    .cloned()
                    .unwrap_or_else(|| inherited_value.clone()),
                inherited_value,
                is_overridden: override_value.is_some(),
            }
        })
        .collect();
    Ok(Json(UserPreferencesEnvelope { preferences }))
}

pub(super) async fn get_preferences(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<UserPreferencesEnvelope>> {
    preferences_envelope(&st.db, &principal.username).await
}

pub(super) async fn patch_preferences(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<serde_json::Map<String, Value>>,
) -> ApiResult<Json<UserPreferencesEnvelope>> {
    let mut changes = Vec::with_capacity(body.len());
    let mut errors = serde_json::Map::new();
    for (key, raw) in body {
        if !PERSONAL_PREFERENCE_KEYS.contains(&key.as_str()) {
            errors.insert(key, json!("unknown personal preference"));
            continue;
        }
        let value = match raw {
            Value::Null => None,
            Value::String(value) => Some(value),
            Value::Number(value) => Some(value.to_string()),
            _ => {
                errors.insert(key, json!("value must be a string, number, or null"));
                continue;
            }
        };
        if let Some(value) = &value {
            if let Err(reason) = config::validate(&key, value) {
                errors.insert(key, json!(reason));
                continue;
            }
        }
        changes.push((key, value));
    }
    if !errors.is_empty() {
        return Err(AppError::bad_request("one or more preferences are invalid")
            .with_details(Value::Object(errors)));
    }
    auth::apply_user_preferences(&st.db, &principal.username, &changes).await?;
    preferences_envelope(&st.db, &principal.username).await
}

fn change_for<'a>(changes: &'a [config::Change], key: &str) -> Option<&'a Option<String>> {
    changes
        .iter()
        .rev()
        .find_map(|(k, v)| (k == key).then_some(v))
}

/// Transitional adapter for pre-profile clients.  These keys are deliberately
/// absent from the settings registry: accepting a PATCH mutates `default`
/// directly, so there is still exactly one launch-policy authority.
async fn apply_legacy_agent_patch(db: &Db, changes: &[config::Change]) -> anyhow::Result<()> {
    let current = profile::get(db, profile::DEFAULT_PROFILE)
        .await?
        .ok_or_else(|| anyhow::anyhow!("default profile is missing"))?;
    let mut input = current.as_input()?;

    if let Some(value) = change_for(changes, "agent.default") {
        input.agent_kind = value
            .as_deref()
            .unwrap_or(config::DEFAULT_AGENT)
            .trim()
            .to_string();
        if change_for(changes, "agent.model").is_none() {
            input.model.clear();
        }
        if change_for(changes, "agent.effort").is_none() {
            input.effort.clear();
        }
        // Protocol defaults are runtime-specific, so re-resolve it whenever
        // the legacy caller changes the runtime.
        input.protocol.clear();
    }
    if let Some(value) = change_for(changes, "agent.model") {
        input.model = value.as_deref().unwrap_or_default().trim().to_string();
    }
    if let Some(value) = change_for(changes, "agent.effort") {
        input.effort = value.as_deref().unwrap_or_default().trim().to_string();
    }
    if let Some(value) = change_for(changes, "agent.mode") {
        input.mode = value
            .as_deref()
            .unwrap_or(config::DEFAULT_AGENT_MODE)
            .trim()
            .to_string();
    }
    profile::upsert(db, &input).await?;
    Ok(())
}
