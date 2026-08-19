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
use weaver_api::operations::agents as ops;
use weaver_api::{
    AgentChoiceView, AgentMetadataView, AgentsView, CustomAgentView, CustomAgentsView,
};

use crate::custom_agents::{self, CustomAgent};
use crate::db::Db;

use super::operations::{register, Bound, OperationContext};
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
    /// Execution backend: `"terminal"` (the default) or `"acp"`. Blank/absent is
    /// normalized to `"terminal"` by [`custom_agents::set`].
    #[serde(default)]
    protocol: String,
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
            protocol: self.protocol.trim().to_string(),
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
    let _resolver = st.launch_gate.acquire_resolver().await;
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
    let _resolver = st.launch_gate.acquire_resolver().await;
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
    let _resolver = st.launch_gate.acquire_resolver().await;
    custom_agents::remove(&st.db, &name).await?;
    custom_envelope(&st.db).await
}

// ---------------------------------------------------------------------------
// Operation registry — `agents.*`, bound onto `weaver_api::operations::agents`.
// Each handler below is the twin of a legacy axum handler above. Authorization
// (`agents.list` is `actor = SessionSelf`; the `custom.*` mutations are
// `actor = Admin`) now happens once, centrally, in `web/operations.rs`. The
// legacy routes above stay live and untouched until the coordinated route
// deletion pass.
// ---------------------------------------------------------------------------

fn agent_choice_view(c: crate::agent::AgentChoice) -> AgentChoiceView {
    AgentChoiceView {
        id: c.id,
        label: c.label,
    }
}

fn agent_metadata_view(m: crate::agent::AgentMetadata) -> AgentMetadataView {
    AgentMetadataView {
        kind: m.kind,
        label: m.label,
        models: m.models.into_iter().map(agent_choice_view).collect(),
        efforts: m.efforts.into_iter().map(agent_choice_view).collect(),
        accepts_raw_model: m.accepts_raw_model,
        supports_hooks: m.supports_hooks,
        builtin: m.builtin,
        supports_acp: m.supports_acp,
        protocol: m.protocol,
    }
}

fn custom_agent_view(a: CustomAgent) -> CustomAgentView {
    CustomAgentView {
        name: a.name,
        label: a.label,
        setup: a.setup,
        launch: a.launch,
        resume: a.resume,
        reports_status: a.reports_status,
        protocol: a.protocol,
        created_at: a.created_at,
        updated_at: a.updated_at,
    }
}

async fn custom_agents_view(db: &Db) -> ApiResult<CustomAgentsView> {
    Ok(CustomAgentsView {
        custom: custom_agents::list(db)
            .await?
            .into_iter()
            .map(custom_agent_view)
            .collect(),
    })
}

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<ops::list::List, _, _>(list_operation),
        register::<ops::custom::create::Create, _, _>(custom_create_operation),
        register::<ops::custom::update::Update, _, _>(custom_update_operation),
        register::<ops::custom::delete::Delete, _, _>(custom_delete_operation),
    ]
}

/// `agents.list` — the twin of [`list_agents`](super::sessions::list_agents)
/// (`web/sessions.rs`, not this file — the picker list has always lived
/// beside session listing; these routes only add/edit/remove the custom rows
/// it merges in). Reimplemented here rather than called, since that handler's
/// module is owned by another agent for the duration of this port; the
/// `agent::agent_metadata` / `custom_agents::list` / `crate::profile` calls
/// below are the same shared domain logic that handler uses.
async fn list_operation(
    context: OperationContext,
    _input: ops::list::Input,
) -> ApiResult<ops::list::Output> {
    let st = context.state;
    let default_agent = crate::profile::get(&st.db, crate::profile::DEFAULT_PROFILE)
        .await?
        .map(|profile| profile.agent_kind)
        .unwrap_or_else(|| crate::config::DEFAULT_AGENT.to_string());
    Ok(AgentsView {
        agents: crate::agent::agent_metadata(&st.db)
            .await?
            .into_iter()
            .map(agent_metadata_view)
            .collect(),
        custom: custom_agents::list(&st.db)
            .await?
            .into_iter()
            .map(custom_agent_view)
            .collect(),
        default_agent,
    })
}

/// `agents.custom.create` — the twin of [`create_custom_agent`].
async fn custom_create_operation(
    context: OperationContext,
    input: ops::custom::create::Input,
) -> ApiResult<ops::custom::create::Output> {
    let st = context.state;
    let _resolver = st.launch_gate.acquire_resolver().await;
    let name = input.name.trim().to_string();
    custom_agents::validate_name(&name).map_err(AppError::bad_request)?;
    if custom_agents::exists(&st.db, &name).await? {
        return Err(AppError::conflict(format!(
            "an agent named '{name}' already exists"
        )));
    }
    let agent = CustomAgent {
        name,
        label: input.label.trim().to_string(),
        setup: input.setup,
        launch: input.launch,
        resume: input.resume,
        reports_status: input.reports_status,
        protocol: input.protocol.trim().to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    custom_agents::validate_fields(&agent).map_err(AppError::bad_request)?;
    custom_agents::set(&st.db, &agent).await?;
    custom_agents_view(&st.db).await
}

/// `agents.custom.update` — the twin of [`update_custom_agent`]. `name` is a
/// caller-supplied operand here rather than a path segment, but is otherwise
/// immutable the same way: it selects the row, and is never taken from the
/// stored/updated fields.
async fn custom_update_operation(
    context: OperationContext,
    input: ops::custom::update::Input,
) -> ApiResult<ops::custom::update::Output> {
    let st = context.state;
    let _resolver = st.launch_gate.acquire_resolver().await;
    if !custom_agents::exists(&st.db, &input.name).await? {
        return Err(AppError::not_found("custom agent"));
    }
    let agent = CustomAgent {
        name: input.name,
        label: input.label.trim().to_string(),
        setup: input.setup,
        launch: input.launch,
        resume: input.resume,
        reports_status: input.reports_status,
        protocol: input.protocol.trim().to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    custom_agents::validate_fields(&agent).map_err(AppError::bad_request)?;
    custom_agents::set(&st.db, &agent).await?;
    custom_agents_view(&st.db).await
}

/// `agents.custom.delete` — the twin of [`delete_custom_agent`].
async fn custom_delete_operation(
    context: OperationContext,
    input: ops::custom::delete::Input,
) -> ApiResult<ops::custom::delete::Output> {
    let st = context.state;
    let _resolver = st.launch_gate.acquire_resolver().await;
    custom_agents::remove(&st.db, &input.name).await?;
    custom_agents_view(&st.db).await
}
