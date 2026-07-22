use axum::{extract::State, Json};
use serde_json::{json, Value};

use crate::config;
use crate::db::Db;
use crate::profile;

use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async fn settings_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "settings": config::describe(db).await? })))
}

pub(super) async fn get_settings(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    settings_envelope(&st.db).await
}

pub(super) async fn patch_settings(
    State(st): State<AppState>,
    Json(body): Json<serde_json::Map<String, Value>>,
) -> ApiResult<Json<Value>> {
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
