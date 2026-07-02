//! CRUD for **custom agents** — the operator-defined agents in the
//! `custom_agents` table that appear in the picker beside the builtin
//! `claude`/`codex`. The picker listing itself is `GET /api/agents`
//! ([`super::sessions::list_agents`]); these routes add/edit/remove the custom
//! rows it merges in.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::custom_agents::{self, CustomAgent};
use crate::db::Db;

use super::{ApiResult, AppError, AppState};

/// The editable fields of a custom agent. `name` is used only by the create
/// route (update takes it from the path); the stage commands default to empty.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CustomAgentBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    setup: String,
    #[serde(default)]
    launch: String,
    #[serde(default)]
    resume: String,
    #[serde(default)]
    reports_status: bool,
}

impl CustomAgentBody {
    /// Assemble a [`CustomAgent`] under `name`. The timestamps are filled in by
    /// [`custom_agents::set`], so they start blank.
    fn into_agent(self, name: &str) -> CustomAgent {
        CustomAgent {
            name: name.to_string(),
            label: self.label.trim().to_string(),
            setup: self.setup,
            launch: self.launch,
            resume: self.resume,
            reports_status: self.reports_status,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

/// The custom-agent list, returned by every mutating route so the caller can
/// refresh in one round trip.
async fn custom_envelope(db: &Db) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "custom": custom_agents::list(db).await? })))
}

/// `POST /api/agents/custom` — define a new custom agent. The name must be a
/// fresh, non-reserved slug and the definition must have a label (the stage
/// commands are optional — a command-less agent execs a bare login shell).
pub(super) async fn create_custom_agent(
    State(st): State<AppState>,
    Json(body): Json<CustomAgentBody>,
) -> ApiResult<Json<Value>> {
    let name = body.name.trim().to_string();
    custom_agents::validate_name(&name).map_err(AppError::bad_request)?;
    if custom_agents::exists(&st.db, &name).await? {
        return Err(AppError::conflict(format!(
            "an agent named '{name}' already exists"
        )));
    }
    let agent = body.into_agent(&name);
    custom_agents::validate_fields(&agent).map_err(AppError::bad_request)?;
    custom_agents::set(&st.db, &agent).await?;
    custom_envelope(&st.db).await
}

/// `PUT /api/agents/custom/{name}` — replace an existing custom agent's
/// definition. The name (from the path) is immutable; a builtin or unknown name
/// is a 404.
pub(super) async fn update_custom_agent(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<CustomAgentBody>,
) -> ApiResult<Json<Value>> {
    if !custom_agents::exists(&st.db, &name).await? {
        return Err(AppError::not_found("custom agent"));
    }
    let agent = body.into_agent(&name);
    custom_agents::validate_fields(&agent).map_err(AppError::bad_request)?;
    custom_agents::set(&st.db, &agent).await?;
    custom_envelope(&st.db).await
}

/// `DELETE /api/agents/custom/{name}` — remove a custom agent. Removing an absent
/// name is a no-op (the desired end state already holds). Sessions already
/// launched with the agent are unaffected; a later adopt of one would fail to
/// resolve it, which surfaces as a clear launch error.
pub(super) async fn delete_custom_agent(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    custom_agents::remove(&st.db, &name).await?;
    custom_envelope(&st.db).await
}
