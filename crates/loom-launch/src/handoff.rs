//! Provider handoff orchestration with one-way dependencies on runtime/domain owners,
//! never Axum or the REST adapter.

use std::path::PathBuf;

use serde_json::json;
use weaver_api::{LaunchOverrides, LaunchSelection, ResolvedLaunchView};
use weaver_core::branch::{self as branch_mod, Branch};
use weaver_core::BoxFut;

use crate::session::{self as session_mod, Session};
use crate::{agent, backend, custom_agents, events, lifecycle, repo, runtime, AppState};
use weaver_api::operations::sessions;

const HANDOFF_SUMMARY_CHARS: usize = 32 * 1024;
const HANDOFF_RECENT_MESSAGES: usize = 8;
const HANDOFF_RECENT_CHARS: usize = 16 * 1024;
const HANDOFF_SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

#[derive(Debug)]
pub enum HandoffError {
    BadRequest(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String, Option<Box<ResolvedLaunchView>>),
    PreconditionRequired(String),
    Internal(String),
}

impl HandoffError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into(), None)
    }

    fn not_found(what: &str) -> Self {
        Self::NotFound(format!("{what} not found"))
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    fn with_preview(self, preview: ResolvedLaunchView) -> Self {
        match self {
            Self::Conflict(message, _) => Self::Conflict(message, Some(Box::new(preview))),
            error => error,
        }
    }
}

impl From<anyhow::Error> for HandoffError {
    fn from(error: anyhow::Error) -> Self {
        Self::internal(error.to_string())
    }
}

type Result<T> = std::result::Result<T, HandoffError>;

fn legacy_handoff_mode(requested: &Option<String>, current: &str) -> String {
    requested
        .as_deref()
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .unwrap_or(current)
        .to_string()
}

fn handoff_selection(req: &sessions::handoff::Input, session: &Session) -> Result<LaunchSelection> {
    if let Some(selection) = &req.selection {
        if !req.agent.trim().is_empty()
            || req.model.is_some()
            || req.effort.is_some()
            || req.mode.is_some()
        {
            return Err(HandoffError::bad_request(
                "canonical handoff selection cannot be combined with flattened agent/model/effort/mode fields",
            ));
        }
        return Ok(selection.clone());
    }
    let target = req.agent.trim();
    if target.is_empty() {
        return Err(HandoffError::bad_request("handoff agent is required"));
    }
    Ok(LaunchSelection {
        profile: session.profile.clone(),
        overrides: LaunchOverrides {
            agent: Some(target.to_string()),
            model: req.model.clone(),
            effort: req.effort.clone(),
            // Absent or blank mode falls back to the live session's permission
            // posture, for the flattened handoff form.
            mode: Some(legacy_handoff_mode(&req.mode, &session.launch_mode)),
            ..Default::default()
        },
    })
}

fn handoff_resolve_options(session: &Session) -> crate::launch::ResolveOptions {
    crate::launch::ResolveOptions {
        // Resolve the selected template's real class. The handoff boundary
        // compares it with the existing session instead of coercing it first.
        default_class: None,
        capacity_credit_profile: crate::profile::status_consumes_capacity(&session.status)
            .then(|| session.profile.clone()),
        ..Default::default()
    }
}

async fn resolve_handoff_selection(
    st: &AppState,
    session: &Session,
    selection: &LaunchSelection,
) -> Result<crate::launch::ResolvedLaunch> {
    let mut resolved = crate::launch::resolve(&st.db, selection, &handoff_resolve_options(session))
        .await
        .map_err(|error| HandoffError::bad_request(error.to_string()))?;
    if resolved.view.class != session.class {
        resolved.view.errors.push(format!(
            "profile '{}' is {}-class; this {} session cannot change class during handoff",
            resolved.profile.name, resolved.view.class, session.class
        ));
    }
    if resolved.view.protocol != "acp" {
        resolved.view.errors.push(format!(
            "agent '{}' does not resolve to the ACP protocol required for handoff",
            resolved.view.agent
        ));
    }
    if resolved.profile.restricted {
        resolved
            .view
            .errors
            .push("restricted profiles cannot be applied by handoff".to_string());
    }
    resolved.view.valid = resolved.view.errors.is_empty();
    Ok(resolved)
}

fn require_handoff_source(session: &Session) -> Result<()> {
    if session.protocol != "acp" {
        return Err(HandoffError::conflict(format!(
            "session '{}' is a terminal session, not an ACP conversation",
            session.id
        )));
    }
    if session.policy_restricted {
        return Err(HandoffError::Forbidden(
            "restricted sessions cannot change agent runtime".to_string(),
        ));
    }
    if !matches!(session.status.as_str(), "running" | "orphaned" | "error") {
        return Err(HandoffError::conflict(format!(
            "session '{}' is {}, not handoff-capable",
            session.id, session.status
        )));
    }
    if session.managed_by.is_some() {
        return Err(HandoffError::conflict(
            "engine-managed sessions cannot be handed off manually",
        ));
    }
    Ok(())
}

pub async fn resolve_session_handoff(
    st: &AppState,
    session: &Session,
    selection: &LaunchSelection,
) -> Result<ResolvedLaunchView> {
    require_handoff_source(session)?;
    let profile_name = match selection.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    };
    let _profile_permit = st.launch_gate.acquire_profile(profile_name).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    Ok(resolve_handoff_selection(st, session, selection)
        .await?
        .view)
}

struct HandoffPlan {
    target: String,
    model: String,
    effort: String,
    mode: String,
    class: String,
    profile: String,
    profile_revision: i64,
    profile_lifetime: i64,
    env_clear: bool,
    ambient_allowlist: String,
    idle_archive_secs: Option<i64>,
    turn_budget: i64,
    prelude: String,
    instructions: String,
    restricted: bool,
    strict: bool,
    github_repositories: String,
    allowed_tools: String,
    mcp_access: String,
    launch_snapshot: String,
    profile_environment: Vec<(String, String)>,
    custom_agent: Option<custom_agents::CustomAgent>,
}

fn legacy_handoff_snapshot(
    session: &Session,
    target: &str,
    model: &str,
    effort: &str,
    mode: &str,
    custom_agent: Option<&custom_agents::CustomAgent>,
) -> Result<String> {
    if session.launch_snapshot.trim().is_empty() {
        return Ok(String::new());
    }
    let mut snapshot = crate::launch::deserialize_snapshot(&session.launch_snapshot)
        .map_err(|error| HandoffError::bad_request(error.to_string()))?;
    snapshot.view.agent = target.to_string();
    snapshot.view.model = model.to_string();
    snapshot.view.effort = effort.to_string();
    snapshot.view.protocol = "acp".to_string();
    snapshot.view.mode = mode.to_string();
    snapshot.custom_agent = custom_agent.cloned();
    snapshot.view.selection.overrides.agent = Some(target.to_string());
    snapshot.view.selection.overrides.model = Some(model.to_string());
    snapshot.view.selection.overrides.effort = Some(effort.to_string());
    snapshot.view.selection.overrides.mode = Some(mode.to_string());
    snapshot.view.provenance.agent = "launch_override".to_string();
    snapshot.view.provenance.model = if model.is_empty() {
        "agent_default"
    } else {
        "launch_override"
    }
    .to_string();
    snapshot.view.provenance.effort = if effort.is_empty() {
        "agent_default"
    } else {
        "launch_override"
    }
    .to_string();
    snapshot.view.provenance.protocol = "agent_default".to_string();
    snapshot.view.provenance.mode = "launch_override".to_string();
    crate::launch::serialize_snapshot(&snapshot.view, snapshot.custom_agent.as_ref())
        .map_err(|error| HandoffError::bad_request(error.to_string()))
}

async fn legacy_handoff_plan(
    st: &AppState,
    req: &sessions::handoff::Input,
    session: &Session,
) -> Result<HandoffPlan> {
    let target = req.agent.trim();
    if target.is_empty() {
        return Err(HandoffError::bad_request("handoff agent is required"));
    }
    let custom_agent = if crate::agent::builtin_agent_type(target).is_some() {
        None
    } else {
        Some(
            custom_agents::get(&st.db, target)
                .await?
                .ok_or_else(|| HandoffError::bad_request(format!("unknown agent '{target}'")))?,
        )
    };
    let metadata = match custom_agent.as_ref() {
        Some(custom) => crate::agent::custom_metadata(custom),
        None => crate::agent::metadata_for(&st.db, target)
            .await?
            .ok_or_else(|| HandoffError::bad_request(format!("unknown agent '{target}'")))?,
    };
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let effort = req
        .effort
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    crate::agent::validate_model(&metadata, &model).map_err(HandoffError::bad_request)?;
    crate::agent::validate_effort(&metadata, &effort).map_err(HandoffError::bad_request)?;
    let protocol =
        crate::agent::resolve_protocol(&metadata, None).map_err(HandoffError::bad_request)?;
    if protocol != "acp" {
        return Err(HandoffError::bad_request(format!(
            "agent '{target}' does not resolve to the ACP protocol required for handoff"
        )));
    }
    let mode = legacy_handoff_mode(&req.mode, &session.launch_mode);
    if !matches!(
        mode.as_str(),
        "auto" | "default" | "acceptEdits" | "plan" | "bypassPermissions"
    ) {
        return Err(HandoffError::bad_request(format!(
            "invalid handoff mode '{mode}'"
        )));
    }
    let lifetime = crate::profile::get_including_retired(&st.db, &session.profile)
        .await?
        .ok_or_else(|| {
            HandoffError::conflict(
                "the session's original profile lifetime is unavailable; review a canonical handoff preview",
            )
        })?;
    if session.profile_lifetime == 0 || lifetime.lifetime != session.profile_lifetime {
        return Err(HandoffError::conflict(
            "the session's profile name now refers to a different template lifetime; review a canonical handoff preview",
        ));
    }
    let keeps_same_slot = crate::profile::status_consumes_capacity(&session.status);
    if lifetime.max_concurrent > 0
        && !keeps_same_slot
        && crate::profile::active_count(&st.db, &session.profile).await? >= lifetime.max_concurrent
    {
        return Err(HandoffError::conflict(format!(
            "profile '{}' has reached its max_concurrent limit ({})",
            session.profile, lifetime.max_concurrent
        )));
    }
    let profile_environment = crate::profile::env_pairs(&st.db, &session.profile)
        .await
        .map_err(|error| HandoffError::bad_request(error.to_string()))?;
    let instructions = if session.launch_snapshot.trim().is_empty() {
        String::new()
    } else {
        crate::launch::deserialize_snapshot(&session.launch_snapshot)
            .map_err(|error| HandoffError::bad_request(error.to_string()))?
            .view
            .policy
            .instructions
    };
    Ok(HandoffPlan {
        target: target.to_string(),
        model: model.clone(),
        effort: effort.clone(),
        mode: mode.clone(),
        class: session.class.clone(),
        profile: session.profile.clone(),
        profile_revision: session.profile_revision,
        profile_lifetime: session.profile_lifetime,
        env_clear: session.policy_env_clear,
        ambient_allowlist: session.policy_ambient_allowlist.clone(),
        idle_archive_secs: session.policy_idle_archive_secs,
        turn_budget: session.policy_turn_budget,
        prelude: session.policy_prelude.clone(),
        instructions,
        restricted: session.policy_restricted,
        strict: session.policy_strict,
        github_repositories: session.policy_github_repositories.clone(),
        allowed_tools: session.policy_allowed_tools.clone(),
        mcp_access: session.policy_mcp_access.clone(),
        launch_snapshot: legacy_handoff_snapshot(
            session,
            target,
            &model,
            &effort,
            &mode,
            custom_agent.as_ref(),
        )?,
        profile_environment,
        custom_agent,
    })
}

/// Replace the provider behind an idle ACP work session while preserving Loom's
/// stable session/branch/worktree identity and canonical journal.
/// Boxed to keep this state machine's codegen in `loom-launch` — see the note
/// on [`crate::provision::create`].
pub fn handoff_session(
    st: &AppState,
    initial_session: Session,
    req: sessions::handoff::Input,
) -> BoxFut<'_, Result<(Session, Branch)>> {
    let st = st.clone();
    Box::pin(async move {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        weaver_core::spawn_boxed(Box::pin(async move {
            let result = handoff_session_inner(&st, initial_session, req).await;
            let _ = result_tx.send(result);
        }));
        result_rx.await.map_err(|_| {
            HandoffError::internal("handoff task stopped before reporting its result")
        })?
    })
}

async fn handoff_session_inner(
    st: &AppState,
    initial_session: Session,
    req: sessions::handoff::Input,
) -> Result<(Session, Branch)> {
    require_handoff_source(&initial_session)?;
    let canonical = req.selection.is_some();
    if canonical
        && (req.expected_profile_revision.is_none() || req.expected_resolver_revision.is_none())
    {
        return Err(HandoffError::bad_request(
            "canonical handoff selection requires expected_profile_revision and expected_resolver_revision from a handoff preview",
        ));
    }
    let _source_permit = st.launch_gate.acquire_session(&initial_session.id).await;
    let _lifecycle = runtime::LIFECYCLE_LOCK.lock().await;
    let Some((session, branch)) = session_mod::with_branch(&st.db, &initial_session.id).await?
    else {
        return Err(HandoffError::conflict(
            "session changed while the handoff request was waiting; review it again",
        ));
    };
    require_handoff_source(&session)?;
    let unchanged_source = session.status == initial_session.status
        && session.agent_kind == initial_session.agent_kind
        && session.model == initial_session.model
        && session.effort == initial_session.effort
        && session.profile == initial_session.profile
        && session.profile_revision == initial_session.profile_revision
        && session.profile_lifetime == initial_session.profile_lifetime
        && session.launch_mode == initial_session.launch_mode
        && session.launch_snapshot == initial_session.launch_snapshot
        && session.mutation_revision == initial_session.mutation_revision;
    if !unchanged_source {
        return Err(HandoffError::conflict(
            "session changed while the handoff request was waiting; review it again",
        ));
    }
    let selection = canonical
        .then(|| handoff_selection(&req, &session))
        .transpose()?;
    let permit_profile = selection
        .as_ref()
        .map(|selection| match selection.profile.trim() {
            "" => crate::profile::DEFAULT_PROFILE,
            name => name,
        })
        .unwrap_or(session.profile.as_str())
        .to_string();
    let _profile_permit = st.launch_gate.acquire_profile(&permit_profile).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    let mut plan = if let Some(selection) = selection {
        let resolved = match resolve_handoff_selection(st, &session, &selection).await {
            Ok(resolved) => resolved,
            Err(HandoffError::BadRequest(message)) if req.expected_resolver_revision.is_some() => {
                return Err(HandoffError::conflict(format!(
                    "handoff settings can no longer be resolved after preview: {}",
                    message
                )));
            }
            Err(error) => return Err(error),
        };
        if req
            .expected_profile_revision
            .is_some_and(|expected| expected != resolved.view.profile_revision)
            || req
                .expected_resolver_revision
                .as_deref()
                .is_some_and(|expected| expected != resolved.view.resolver_revision)
        {
            return Err(HandoffError::conflict(
                "handoff settings changed after preview; review the fresh resolution",
            )
            .with_preview(resolved.view));
        }
        if !resolved.view.valid {
            return Err(HandoffError::conflict(
                "resolved handoff settings are not currently launchable",
            )
            .with_preview(resolved.view));
        }
        let profile_environment = crate::profile::env_pairs(&st.db, &resolved.profile.name)
            .await
            .map_err(|error| HandoffError::bad_request(error.to_string()))?;
        let launch_snapshot =
            crate::launch::serialize_snapshot(&resolved.view, resolved.custom_agent.as_ref())
                .map_err(|error| HandoffError::bad_request(error.to_string()))?;
        HandoffPlan {
            target: resolved.view.agent.clone(),
            model: resolved.view.model.clone(),
            effort: resolved.view.effort.clone(),
            mode: resolved.view.mode.clone(),
            class: resolved.view.class.clone(),
            profile: resolved.profile.name.clone(),
            profile_revision: resolved.profile.revision,
            profile_lifetime: resolved.profile.lifetime,
            env_clear: resolved.profile.env_clear,
            ambient_allowlist: resolved.profile.ambient_allowlist.clone(),
            idle_archive_secs: resolved.view.policy.idle_archive_secs,
            turn_budget: resolved.view.policy.turn_budget.unwrap_or(0),
            prelude: resolved.profile.prelude.clone(),
            instructions: resolved.profile.instructions.clone(),
            restricted: resolved.profile.restricted,
            strict: resolved.profile.strict,
            github_repositories: resolved.profile.github_repositories.clone(),
            allowed_tools: serde_json::to_string(&resolved.runtime_permissions)
                .map_err(|error| HandoffError::bad_request(error.to_string()))?,
            mcp_access: serde_json::to_string(&resolved.mcp_policy)
                .map_err(|error| HandoffError::bad_request(error.to_string()))?,
            launch_snapshot,
            profile_environment,
            custom_agent: resolved.custom_agent,
        }
    } else {
        legacy_handoff_plan(st, &req, &session).await?
    };
    let target = plan.target.clone();
    let model = plan.model.clone();
    let effort = plan.effort.clone();
    let mode = plan.mode.clone();
    if target == session.agent_kind
        && model == session.model
        && effort == session.effort
        && plan.profile == session.profile
        && plan.profile_revision == session.profile_revision
        && mode == session.launch_mode
    {
        return Err(HandoffError::bad_request(
            "handoff target matches the current runtime profile",
        ));
    }
    // Resolve every fallible launch input before quiescing the current task.
    let repo_root = PathBuf::from(&branch.repo_root);
    let configured_github_repositories: Vec<String> =
        serde_json::from_str(&plan.github_repositories)
            .map_err(|error| HandoffError::bad_request(error.to_string()))?;
    let current_github_repo = repo::github_slug_for_root(&st.db, &repo_root).await?;
    let session_github_repositories = runtime::session_github_repositories(
        &plan.class,
        &configured_github_repositories,
        current_github_repo.as_deref(),
    );
    plan.github_repositories = serde_json::to_string(&session_github_repositories)
        .map_err(|error| HandoffError::bad_request(error.to_string()))?;
    let handoff_policy = session_mod::SessionHandoffPolicy {
        agent_kind: target.clone(),
        model: model.clone(),
        effort: effort.clone(),
        profile: plan.profile.clone(),
        launch_mode: mode.clone(),
        profile_revision: plan.profile_revision,
        profile_lifetime: plan.profile_lifetime,
        strict: plan.strict,
        env_clear: plan.env_clear,
        ambient_allowlist: plan.ambient_allowlist.clone(),
        idle_archive_secs: plan.idle_archive_secs,
        turn_budget: plan.turn_budget,
        prelude: plan.prelude.clone(),
        restricted: plan.restricted,
        github_repositories: plan.github_repositories.clone(),
        allowed_tools: plan.allowed_tools.clone(),
        mcp_access: plan.mcp_access.clone(),
        launch_snapshot: plan.launch_snapshot.clone(),
    };
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(HandoffError::bad_request(format!(
            "worktree {} no longer exists on disk — cannot hand off",
            session.work_dir
        )));
    }
    let repo_cfg = runtime::repo_cfg_or_default(&repo_root);
    let mut extra_env = runtime::layer_launch_environment(
        &st.db,
        &repo_root,
        &repo_cfg,
        &plan.profile,
        plan.profile_environment.clone(),
        plan.strict,
        plan.restricted,
    )
    .await;
    if plan.env_clear {
        let allowlist: Vec<String> = serde_json::from_str(&plan.ambient_allowlist)
            .map_err(|error| HandoffError::bad_request(error.to_string()))?;
        extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
    }
    let github_app = runtime::app_for_allowlist(&session_github_repositories, st.trigger.app());
    if current_github_repo.is_some()
        && !runtime::github_credential_available(
            &st.db,
            session.created_by.as_deref(),
            github_app,
            runtime::user_github_token_allowed(&plan.class, plan.restricted),
        )
        .await?
    {
        return Err(HandoffError::PreconditionRequired(
            runtime::MISSING_GITHUB_TOKEN_MESSAGE.to_string(),
        ));
    }
    if !session_mod::begin_transition(&st.db, &session.id, "handoff", "Pausing session").await? {
        return Err(HandoffError::conflict(
            "another lifecycle transition already owns this session",
        ));
    }
    lifecycle::record_transition(st, &branch, "handoff", "Pausing session").await;

    let result: Result<(Session, Branch)> = async {
    // A healthy task quiesces on its ordered command channel, preserving the
    // active-turn/queue safety gate. A missing task is the recovery case: settle
    // its persisted in-flight turn, retain the durable queue, and continue.
    let snapshot = if let Some(handle) = st.acp.get(&session.id) {
        match handle.prepare_handoff().await {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tokio::task::yield_now().await;
                if st.acp.is_live(&session.id) {
                    return Err(HandoffError::conflict(error.to_string()));
                }
                tracing::warn!(session = %session.id, %error,
                    "ACP task vanished while preparing handoff; using persisted recovery state");
                None
            }
        }
    } else {
        tracing::warn!(session = %session.id,
            "handing off without a live ACP task; using persisted recovery state");
        None
    };
    let source_task_quiesced = snapshot.is_some();
    lifecycle::transition_step(
        st,
        &session,
        &branch,
        "handoff",
        &format!("Transferring context to {target}"),
    )
    .await?;
    // Re-read after the task handshake: it may have vanished after our initial
    // route snapshot while persisting a newer in-flight turn.
    let persisted = session_mod::get(&st.db, &session.id)
        .await?
        .ok_or_else(|| HandoffError::not_found("session"))?;
    if let Some(turn) = session_mod::acp_inflight_turn(&persisted) {
        crate::chat::close_abandoned_turn(&st.db, &session.id, turn).await?;
    }
    let blocks = match snapshot {
        Some(blocks) => blocks,
        None => crate::chat::list(&st.db, &session.id).await?,
    };
    let current_goal = branch_mod::current_goal(&st.db, &branch).await?;
    let context = crate::chat::handoff_context(
        &current_goal,
        &blocks,
        HANDOFF_SUMMARY_CHARS,
        HANDOFF_RECENT_MESSAGES,
        HANDOFF_RECENT_CHARS,
    );
    // Only after the source provider has accepted the handoff preflight do we
    // mint a replacement credential. The old credential remains valid until
    // the replacement policy commits; every failure below revokes only this
    // staged token.
    let staged_token = crate::auth::stage_session_token_with_policy(
        &st.db,
        session.created_by.as_deref(),
        &session.id,
        &session.branch_id,
        plan.restricted,
        &plan.mcp_access,
    )
    .await?;
    runtime::configure_session_github_auth(
        &st.db,
        &mut extra_env,
        session.created_by.as_deref(),
        &plan.class,
        plan.restricted,
        github_app,
    )
    .await;
    runtime::set_env(&mut extra_env, "LOOM_TOKEN", staged_token.value.clone());
    runtime::set_env(&mut extra_env, "LOOM_SESSION_ID", session.id.clone());
    let mut launch = match agent::build_acp_launch(
        &st.db,
        &agent::AcpLaunchSpec {
            session_id: &session.id,
            branch_id: &branch.id,
            runtime: &target,
            work_dir: &work_dir,
            server_addr: &st.addr,
            model: &model,
            effort: &effort,
            goal_file: None,
            primer_file: None,
            extra_env: &extra_env,
            env_clear: plan.env_clear,
            mode: &mode,
            prelude: &plan.prelude,
            restricted: plan.restricted,
            allowed_tools: &handoff_policy.allowed_tools,
            mcp_access: &handoff_policy.mcp_access,
            custom: plan.custom_agent.as_ref(),
        },
        agent::AcpOpen::Fresh,
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
                .await
                .ok();
            if source_task_quiesced {
                crate::acp::attach(&st.acp_ctx(), &session.id).await.ok();
            }
            return Err(HandoffError::internal(error.to_string()));
        }
    };
    let digest = agent::AgentManager::new(&st.db, &st.acp)
        .summarize_handoff(
            &target,
            &context.summary_request,
            &launch,
            HANDOFF_SUMMARY_TIMEOUT,
        )
        .await;
    let mut handoff_prompt = crate::chat::handoff_prompt(
        &current_goal,
        digest.text.as_deref(),
        &context.recent_dialogue,
    );
    if let Some(instructions) = crate::provision::profile_instructions_section(&plan.instructions) {
        handoff_prompt.push_str("\n\n");
        handoff_prompt.push_str(&instructions);
    }
    launch.goal = Some(handoff_prompt);
    // The source may emit its final idle lifecycle edge while acknowledging
    // preflight. It is quiesced now, so fence provider replacement against this
    // post-handshake generation rather than the route's earlier snapshot.
    let claimed_generation = persisted.mutation_revision + 1;
    let Some(source_state) =
        session_mod::claim_handoff(&st.db, &session.id, persisted.mutation_revision).await?
    else {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        if source_task_quiesced {
            let current = session_mod::get(&st.db, &session.id).await?;
            if current
                .as_ref()
                .is_some_and(|current| !session_mod::is_terminal(&current.status))
            {
                crate::acp::attach(&st.acp_ctx(), &session.id).await.map_err(|error| {
                    HandoffError::internal(format!(
                        "session changed before provider replacement, and its source task could not be restored: {error}",
                    ))
                })?;
            }
        }
        return Err(HandoffError::conflict(
            "session changed before the handoff could replace its provider; review it again",
        ));
    };
    if let Err(kill_error) = backend::kill_session_and_wait(&session.term_session).await {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        if backend::has_session(&session.term_session).await {
            match session_mod::rollback_handoff_claim(
                &st.db,
                &session.id,
                claimed_generation,
                &source_state,
            )
            .await?
            {
                Some(restored_generation) => {
                    if let Err(attach_error) = crate::acp::attach(&st.acp_ctx(), &session.id).await
                    {
                        session_mod::fail_handoff_claim(&st.db, &session.id, restored_generation)
                            .await
                            .ok();
                        return Err(HandoffError::internal(format!(
                            "source provider teardown failed ({kill_error}); durable state was restored but reattach failed ({attach_error}), so the session was marked error"
                        )));
                    }
                    return Err(HandoffError::internal(format!(
                        "source provider teardown failed; the original provider was restored: {kill_error}"
                    )));
                }
                None => {
                    return Err(HandoffError::conflict(
                        "session changed while failed handoff teardown was rolling back; the newer state was preserved",
                    ));
                }
            }
        }
        session_mod::fail_handoff_claim(&st.db, &session.id, claimed_generation)
            .await
            .ok();
        return Err(HandoffError::internal(format!(
            "source provider teardown failed after the provider disappeared; the session was marked recoverable error: {kill_error}"
        )));
    }
    if !session_mod::clear_claimed_handoff_source(&st.db, &session.id, claimed_generation).await? {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        return Err(HandoffError::conflict(
            "session changed after source teardown; the newer state was preserved",
        ));
    }
    if let Err(error) = crate::chat::reset_usage(&st.db, &session.id).await {
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        session_mod::fail_handoff_claim(&st.db, &session.id, claimed_generation)
            .await
            .ok();
        return Err(error.into());
    }

    let boundary = json!({
        "from": session.agent_kind,
        "to": target,
        "model": model,
        "effort": effort,
        "prompt_version": crate::chat::HANDOFF_PROMPT_VERSION,
        "summary_status": digest.status,
        "summary_model": digest.model,
        "summary": digest.text,
        "through_turn": context.through.map(|(turn, _)| turn),
        "through_seq": context.through.map(|(_, seq)| seq),
    });
    if let Err(error) =
        crate::acp::start_handoff(&st.acp_ctx(), &session.id, launch, boundary).await
    {
        st.acp.stop(&session.id);
        backend::kill_session(&session.term_session).await.ok();
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        let failure_committed = session_mod::prepare_handoff(
            &st.db,
            &session.id,
            "error",
            &handoff_policy,
            claimed_generation,
        )
        .await
        .unwrap_or(false);
        if failure_committed {
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "status",
                json!({ "status": "error", "reason": "agent handoff failed" }),
            )
            .await
            .ok();
        }
        return Err(HandoffError::internal(format!(
            "agent handoff failed: {error}"
        )));
    }
    if !session_mod::prepare_handoff(
        &st.db,
        &session.id,
        "running",
        &handoff_policy,
        claimed_generation,
    )
    .await?
    {
        st.acp.stop(&session.id);
        backend::kill_session(&session.term_session).await.ok();
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        return Err(HandoffError::conflict(
            "session changed while the replacement provider was starting; the newer state was preserved",
        ));
    }
    if let Err(error) =
        crate::auth::commit_staged_session_token(&st.db, &session.id, &staged_token.id).await
    {
        st.acp.stop(&session.id);
        backend::kill_session(&session.term_session).await.ok();
        crate::auth::revoke_staged_session_token(&st.db, &staged_token.id)
            .await
            .ok();
        session_mod::fail_handoff_claim(&st.db, &session.id, claimed_generation + 1)
            .await
            .ok();
        return Err(HandoffError::internal(format!(
            "replacement provider token could not be committed: {error}"
        )));
    }
    if !session_mod::clear_transition(&st.db, &session.id, "handoff").await? {
        return Err(HandoffError::conflict(
            "handoff lost ownership of its lifecycle transition",
        ));
    }

    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "handoff",
        json!({ "from": session.agent_kind, "to": target, "model": model, "effort": effort }),
    )
    .await
    .ok();
    session_mod::with_branch(&st.db, &session.id)
        .await?
        .ok_or_else(|| HandoffError::not_found("session"))
    }
    .await;

    if result.is_err() {
        match session_mod::clear_transition(&st.db, &session.id, "handoff").await {
            Ok(true) => {
                events::record(
                    &st.db,
                    &st.bus,
                    &branch.id,
                    "handoff",
                    json!({ "state": "stopped" }),
                )
                .await
                .ok();
            }
            Ok(false) => {
                tracing::warn!(session = %session.id, "handoff error cleanup no longer owned its transition");
            }
            Err(error) => {
                tracing::warn!(session = %session.id, %error, "handoff error cleanup could not clear its transition");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::legacy_handoff_mode;

    #[test]
    fn legacy_handoff_inherits_current_mode_when_omitted_or_blank() {
        assert_eq!(legacy_handoff_mode(&None, "acceptEdits"), "acceptEdits");
        assert_eq!(
            legacy_handoff_mode(&Some(" ".to_string()), "acceptEdits"),
            "acceptEdits"
        );
        assert_eq!(
            legacy_handoff_mode(&Some(" plan ".to_string()), "acceptEdits"),
            "plan"
        );
    }
}
