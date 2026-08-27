//! CRUD for **custom agents** — the operator-defined agents in the
//! `custom_agents` table that appear in the picker beside the builtin
//! `claude`/`codex`. The picker listing itself is `agents.list`, bound
//! below; these routes add/edit/remove the custom rows it merges in.

use weaver_api::operations::agents as ops;
use weaver_api::{
    AgentChoiceView, AgentMetadataView, AgentsView, CustomAgentView, CustomAgentsView,
};

use crate::custom_agents::{self, CustomAgent};
use crate::db::Db;

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError};

// ---------------------------------------------------------------------------
// Operation registry — `agents.*`, bound onto `weaver_api::operations::agents`.
// Authorization (`agents.list` is `actor = SessionSelf`; the `custom.*`
// mutations are `actor = Admin`) happens once, centrally, in
// `web/operations.rs`.
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
        register::<ops::list::Op, _, _>(list_operation),
        register::<ops::custom::create::Op, _, _>(custom_create_operation),
        register::<ops::custom::update::Op, _, _>(custom_update_operation),
        register::<ops::custom::delete::Op, _, _>(custom_delete_operation),
    ]
}

/// `agents.list` — merges the agent metadata registry with custom agents
/// from the database.
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

/// `agents.custom.update` — `name` selects the row and is immutable.
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

async fn custom_delete_operation(
    context: OperationContext,
    input: ops::custom::delete::Input,
) -> ApiResult<ops::custom::delete::Output> {
    let st = context.state;
    let _resolver = st.launch_gate.acquire_resolver().await;
    custom_agents::remove(&st.db, &input.name).await?;
    custom_agents_view(&st.db).await
}
