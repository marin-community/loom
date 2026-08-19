use serde_json::{json, Value};
use weaver_api::operations::preferences as preferences_operations;
use weaver_api::operations::settings as settings_operations;
use weaver_api::{SettingsEnvelope, UserPreferenceView, UserPreferencesEnvelope};

use crate::auth;
use crate::config;
use crate::db::Db;
use crate::profile;

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

const PERSONAL_PREFERENCE_KEYS: &[&str] =
    &["terminal.theme", "terminal.font", "terminal.font_size"];

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async fn settings_envelope_core(db: &Db) -> ApiResult<SettingsEnvelope> {
    let settings = config::describe(db)
        .await?
        .into_iter()
        .map(|setting| setting.into())
        .collect();
    Ok(SettingsEnvelope { settings })
}

/// `settings.get`.
pub(super) async fn get_settings_operation(
    context: OperationContext,
    _input: settings_operations::get::Input,
) -> ApiResult<SettingsEnvelope> {
    settings_envelope_core(&context.state.db).await
}

/// Apply accepted changes (already allowlist-checked) and return the refreshed
/// envelope. Split out from `settings.patch` because the `agent.*`
/// compatibility keys mutate the default profile instead of the settings table
/// and so need the launch gate held across both writes.
async fn apply_settings_changes(
    st: &AppState,
    changes: Vec<config::Change>,
    legacy_agent_changes: Vec<config::Change>,
) -> ApiResult<SettingsEnvelope> {
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
    settings_envelope_core(&st.db).await
}

/// A setting value as a caller writes it, reduced to the string it is stored as.
///
/// `None` (a JSON `null`) clears the key. An array or an object is not a setting
/// value and is refused by key rather than stringified into nonsense.
fn setting_value(raw: Option<Value>) -> Result<Option<String>, &'static str> {
    match raw {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(Value::Bool(flag)) => Ok(Some(flag.to_string())),
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        Some(_) => Err("value must be a string, number, boolean, or null"),
    }
}

/// `settings.patch`. A key must be a registered setting (`config::spec`) or one
/// of the four legacy `agent.*` compatibility keys.
pub(super) async fn patch_settings_operation(
    context: OperationContext,
    input: settings_operations::patch::Input,
) -> ApiResult<SettingsEnvelope> {
    let mut changes: Vec<config::Change> = Vec::with_capacity(input.changes.len());
    let mut legacy_agent_changes: Vec<config::Change> = Vec::new();
    let mut errors = serde_json::Map::new();

    for (key, raw) in input.changes {
        let legacy_agent = matches!(
            key.as_str(),
            "agent.default" | "agent.model" | "agent.effort" | "agent.mode"
        );
        if config::spec(&key).is_none() && !legacy_agent {
            errors.insert(key, json!("unknown setting"));
            continue;
        }
        let value = match setting_value(raw) {
            Ok(value) => value,
            Err(why) => {
                errors.insert(key, json!(why));
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
    apply_settings_changes(&context.state, changes, legacy_agent_changes).await
}

async fn preferences_envelope(db: &Db, username: &str) -> ApiResult<UserPreferencesEnvelope> {
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
    Ok(UserPreferencesEnvelope { preferences })
}

/// `preferences.get`.
async fn get_preferences_operation(
    context: OperationContext,
    _input: preferences_operations::get::Input,
) -> ApiResult<UserPreferencesEnvelope> {
    preferences_envelope(&context.state.db, &context.principal.username).await
}

/// `preferences.patch`. The three personal keys are the whole allowlist; a
/// value that is not a JSON scalar, or that `config::validate` refuses for its
/// key, fails the whole patch rather than applying the rest.
async fn patch_preferences_operation(
    context: OperationContext,
    input: preferences_operations::patch::Input,
) -> ApiResult<UserPreferencesEnvelope> {
    let mut changes = Vec::with_capacity(input.changes.len());
    let mut errors = serde_json::Map::new();
    for (key, raw) in input.changes {
        if !PERSONAL_PREFERENCE_KEYS.contains(&key.as_str()) {
            errors.insert(key, json!("unknown personal preference"));
            continue;
        }
        let value = match setting_value(raw) {
            Ok(value) => value,
            Err(why) => {
                errors.insert(key, json!(why));
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
    let db = &context.state.db;
    let username = &context.principal.username;
    auth::apply_user_preferences(db, username, &changes).await?;
    preferences_envelope(db, username).await
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

// ---------------------------------------------------------------------------
// Operation registry — `settings.*`, bound onto
// `weaver_api::operations::settings`. `settings.env.*` handlers live in
// `web/env.rs` (the file that already owns the default profile's environment
// facade); this bundle just registers them alongside `settings.get`/`.patch`.
// `preferences.*` is a separate bundle — per-operator overrides rather than
// server-wide configuration, and human-but-not-admin where `settings.patch` is
// admin — but its two handlers sit here, beside the settings ones they share
// `config::validate` and `setting_value` with.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<settings_operations::get::Get, _, _>(get_settings_operation),
        register::<settings_operations::patch::Patch, _, _>(patch_settings_operation),
        register::<settings_operations::env::list::List, _, _>(
            super::env::list_settings_env_operation,
        ),
        register::<settings_operations::env::set::Set, _, _>(
            super::env::set_settings_env_operation,
        ),
        register::<settings_operations::env::delete::Delete, _, _>(
            super::env::delete_settings_env_operation,
        ),
        register::<preferences_operations::get::Get, _, _>(get_preferences_operation),
        register::<preferences_operations::patch::Patch, _, _>(patch_preferences_operation),
    ]
}
