use axum::{extract::State, Json};
use serde_json::{json, Value};

use crate::agent;
use crate::config;
use crate::db::Db;

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
    let mut errors = serde_json::Map::new();

    for (key, raw) in body {
        if config::spec(&key).is_none() {
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
        if let Some(value) = &value {
            if let Err(why) = config::validate(&key, value) {
                errors.insert(key, json!(why));
                continue;
            }
        }
        changes.push((key, value));
    }
    if errors.is_empty() {
        validate_agent_settings_patch(&st.db, &mut changes, &mut errors).await;
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
    config::apply(&st.db, &changes).await?;
    settings_envelope(&st.db).await
}

fn change_for<'a>(changes: &'a [config::Change], key: &str) -> Option<&'a Option<String>> {
    changes
        .iter()
        .rev()
        .find_map(|(k, v)| (k == key).then_some(v))
}

fn changed(changes: &[config::Change], key: &str) -> bool {
    change_for(changes, key).is_some()
}

async fn setting_after(db: &Db, changes: &[config::Change], key: &str, default: &str) -> String {
    match change_for(changes, key) {
        Some(Some(value)) => value.trim().to_string(),
        Some(None) => default.to_string(),
        None => config::get_or(db, key, default).await.trim().to_string(),
    }
}

async fn validate_agent_settings_patch(
    db: &Db,
    changes: &mut Vec<config::Change>,
    errors: &mut serde_json::Map<String, Value>,
) {
    validate_agent_settings_group(
        db,
        changes,
        errors,
        AgentSettingsGroup {
            agent_key: "agent.default",
            agent_default: config::DEFAULT_AGENT,
            model_key: "agent.model",
            effort_key: "agent.effort",
            require_concierge: false,
        },
    )
    .await;
    validate_agent_settings_group(
        db,
        changes,
        errors,
        AgentSettingsGroup {
            agent_key: "concierge.runtime",
            agent_default: config::DEFAULT_CONCIERGE_RUNTIME,
            model_key: "concierge.model",
            effort_key: "concierge.effort",
            require_concierge: true,
        },
    )
    .await;
}

struct AgentSettingsGroup {
    agent_key: &'static str,
    agent_default: &'static str,
    model_key: &'static str,
    effort_key: &'static str,
    require_concierge: bool,
}

async fn validate_agent_settings_group(
    db: &Db,
    changes: &mut Vec<config::Change>,
    errors: &mut serde_json::Map<String, Value>,
    group: AgentSettingsGroup,
) {
    if !changed(changes, group.agent_key)
        && !changed(changes, group.model_key)
        && !changed(changes, group.effort_key)
    {
        return;
    }

    let agent_kind = setting_after(db, changes, group.agent_key, group.agent_default).await;
    let Some(agent_type) = agent::agent_type(&agent_kind) else {
        errors.insert(
            group.agent_key.to_string(),
            json!(format!("unknown agent '{agent_kind}'")),
        );
        return;
    };
    let metadata = agent_type.metadata();
    if group.require_concierge && !metadata.supports_concierge {
        errors.insert(
            group.agent_key.to_string(),
            json!(format!("agent '{agent_kind}' cannot run the concierge")),
        );
        return;
    }

    let model = setting_after(db, changes, group.model_key, "").await;
    validate_agent_selector_setting(
        changes,
        errors,
        group.agent_key,
        group.model_key,
        &model,
        || agent_type.validate(&model, ""),
    );

    let effort = setting_after(db, changes, group.effort_key, "").await;
    validate_agent_selector_setting(
        changes,
        errors,
        group.agent_key,
        group.effort_key,
        &effort,
        || agent_type.validate("", &effort),
    );
}

fn validate_agent_selector_setting(
    changes: &mut Vec<config::Change>,
    errors: &mut serde_json::Map<String, Value>,
    agent_key: &str,
    selector_key: &str,
    value: &str,
    validate: impl FnOnce() -> std::result::Result<(), String>,
) {
    if value.is_empty() {
        return;
    }
    if let Err(why) = validate() {
        if changed(changes, agent_key) && !changed(changes, selector_key) {
            changes.push((selector_key.to_string(), None));
        } else {
            errors.insert(selector_key.to_string(), json!(why));
        }
    }
}
