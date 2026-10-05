use std::path::PathBuf;

use serde_json::{json, Value};
use weaver_api::operations::agents as agents_operations;
use weaver_api::operations::watches as watches_operations;
use weaver_api::{ProgramView, WatchDeleteResult, WatchRunResult, WatchRunView, WatchView};
use weaver_core::watch::{self as watch_store, Watch};

use crate::agent;
use crate::db::Db;
use crate::watch as ov_engine;

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Watches — operator and authoring operations (server-owned state)
// ---------------------------------------------------------------------------
//
// Every handler here has a `*_operation` counterpart registered below —
// including `agent_oneshot` (`POST /agent/oneshot`), which is `agents.oneshot`
// instead of a `watches.*` id, since it is a general one-shot ACP primitive
// that watch programs happen to be the first caller of, not watch-specific
// state.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<watches_operations::preview::Op, _, _>(preview_operation),
        register::<watches_operations::occurrences::Op, _, _>(occurrences_operation),
        register::<watches_operations::state::Op, _, _>(state_operation),
        register::<watches_operations::list::Op, _, _>(list_watches_operation),
        register::<watches_operations::get::Op, _, _>(get_watch_operation),
        register::<watches_operations::programs::Op, _, _>(programs_operation),
        register::<watches_operations::create::Op, _, _>(create_watch_operation),
        register::<watches_operations::update::Op, _, _>(update_watch_operation),
        register::<watches_operations::delete::Op, _, _>(delete_watch_operation),
        register::<watches_operations::run::Op, _, _>(run_watch_operation),
        register::<watches_operations::runs::Op, _, _>(watch_runs_operation),
        register::<agents_operations::oneshot::Op, _, _>(agent_oneshot_operation),
    ]
}

/// Build an [`WatchView`] for a watch, joining the most recent
/// round's outcome from the run history.
async fn watch_view(db: &Db, o: &Watch) -> ApiResult<WatchView> {
    let last_outcome = watch_store::recent_runs(db, &o.id, 1)
        .await?
        .into_iter()
        .next()
        .map(|r| r.outcome);
    Ok(WatchView::from_parts(o, last_outcome)?)
}

/// Reject a capability set that isn't a subset of the known ladder, naming the
/// offender. Returns the cleaned set on success.
fn validate_capabilities(caps: &[String]) -> ApiResult<()> {
    for c in caps {
        if !watch_store::CAPABILITIES.contains(&c.as_str()) {
            return Err(AppError::bad_request(format!(
                "unknown capability '{c}' — expected a subset of {}",
                watch_store::CAPABILITIES.join(", ")
            )));
        }
    }
    Ok(())
}

/// A program reference must be a known `builtin:<name>` program or an absolute
/// path (a file under `~/.weaver/watches/`). An unknown builtin is rejected
/// here, naming the registry, rather than erroring every round; a bare relative
/// path is rejected so the engine never resolves it against an ambiguous cwd.
fn validate_program(program: &str) -> ApiResult<()> {
    if program.starts_with("builtin:") {
        if crate::builtins::find(program).is_none() {
            let known = crate::builtins::BUILTINS
                .iter()
                .map(|b| b.program())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(AppError::bad_request(format!(
                "unknown builtin program '{program}' — expected one of {known}"
            )));
        }
        return Ok(());
    }
    if !PathBuf::from(program).is_absolute() {
        return Err(AppError::bad_request(format!(
            "invalid program '{program}' — expected 'builtin:<name>' or an absolute path"
        )));
    }
    Ok(())
}

fn program_views() -> Vec<ProgramView> {
    crate::builtins::BUILTINS.iter().map(|b| b.view()).collect()
}

pub(super) async fn programs_operation(
    _context: OperationContext,
    _input: watches_operations::programs::Input,
) -> ApiResult<Vec<ProgramView>> {
    Ok(program_views())
}

/// Resolve a watch (by id or name) or 404.
async fn require_watch(db: &Db, key: &str) -> ApiResult<Watch> {
    watch_store::resolve(db, key)
        .await?
        .ok_or_else(|| AppError::not_found("watch"))
}

async fn list_watches_core(st: &AppState) -> ApiResult<Vec<WatchView>> {
    let mut out = Vec::new();
    for o in watch_store::list(&st.db).await? {
        out.push(watch_view(&st.db, &o).await?);
    }
    Ok(out)
}

pub(super) async fn list_watches_operation(
    context: OperationContext,
    _input: watches_operations::list::Input,
) -> ApiResult<Vec<WatchView>> {
    list_watches_core(&context.state).await
}

pub(super) async fn create_watch_core(
    st: &AppState,
    req: watches_operations::create::Input,
) -> ApiResult<WatchView> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("name must not be empty"));
    }
    if watch_store::get_by_name(&st.db, &name).await?.is_some() {
        return Err(AppError::conflict(format!(
            "a watch named '{name}' already exists"
        )));
    }

    let defaults = watch_store::NewWatch::default();
    let program = req.program.unwrap_or(defaults.program);
    validate_program(&program)?;
    let capabilities = req.capabilities.unwrap_or_else(|| {
        crate::builtins::find(&program).map_or(defaults.capabilities, |builtin| {
            builtin
                .default_capabilities
                .iter()
                .map(|cap| (*cap).to_string())
                .collect()
        })
    });
    validate_capabilities(&capabilities)?;
    let profile = req
        .agent
        .as_ref()
        .map(|a| a.profile.clone())
        .or(req.profile)
        .unwrap_or(defaults.profile);
    validate_watch_profile(&st.db, &profile).await?;
    let params = json_text(req.params, &defaults.params);

    // The script declares what wakes it: evaluate its subscription manifest
    // (register mode) unless the caller pinned an explicit trigger.
    let trigger_spec = match req.trigger {
        Some(t) => t.to_string(),
        None if req.agent.is_some() => "{}".to_string(),
        None => {
            let params_value = serde_json::from_str(&params).unwrap_or_else(|_| json!({}));
            let fallback = program_default_trigger(&program).unwrap_or(defaults.trigger_spec);
            reconcile_trigger(st, &program, &params_value, &fallback).await
        }
    };

    validate_agent_definition(
        st,
        &trigger_spec,
        req.agent.as_ref(),
        req.late_grace_secs,
        req.run_timeout_secs,
    )
    .await?;
    let new = watch_store::NewWatch {
        name,
        trigger_spec,
        scope: json_text(req.scope, &defaults.scope),
        program,
        params,
        capabilities,
        profile,
        model: req.model.unwrap_or(defaults.model),
        effort: req.effort.unwrap_or(defaults.effort),
        cooldown_secs: req.cooldown_secs.unwrap_or(defaults.cooldown_secs),
        enabled: req.enabled.unwrap_or(defaults.enabled),
        agent: req.agent,
        misfire_policy: req.misfire_policy.unwrap_or_default(),
        late_grace_secs: req.late_grace_secs.unwrap_or(600),
        run_timeout_secs: req.run_timeout_secs.unwrap_or(300),
    };
    let o = watch_store::create(&st.db, &new).await?;
    tracing::info!(watch = %o.id, name = %o.name, "watch created");
    watch_view(&st.db, &o).await
}

pub(super) async fn create_watch_operation(
    context: OperationContext,
    input: watches_operations::create::Input,
) -> ApiResult<WatchView> {
    create_watch_core(&context.state, input).await
}

/// The program's default trigger (a builtin's suggested manifest), used as the
/// fallback when register-mode manifest evaluation declares none or fails.
fn program_default_trigger(program: &str) -> Option<String> {
    crate::builtins::find(program).map(|b| b.default_trigger.to_string())
}

/// Resolve a program's stored trigger from its register-mode manifest, falling
/// back to `fallback` when the script declares no manifest or evaluation fails
/// (a missing interpreter, a syntax error) — best-effort, never an error that
/// blocks creating the watch.
async fn reconcile_trigger(st: &AppState, program: &str, params: &Value, fallback: &str) -> String {
    match ov_engine::evaluate_manifest(st, program, params).await {
        Ok(Some(t)) => serde_json::to_string(&t).unwrap_or_else(|_| fallback.to_string()),
        Ok(None) => fallback.to_string(),
        Err(e) => {
            tracing::debug!(program, error = %e, "manifest evaluation failed; using default trigger");
            fallback.to_string()
        }
    }
}

async fn get_watch_core(st: &AppState, key: &str) -> ApiResult<WatchView> {
    let o = require_watch(&st.db, key).await?;
    watch_view(&st.db, &o).await
}

pub(super) async fn get_watch_operation(
    context: OperationContext,
    input: watches_operations::get::Input,
) -> ApiResult<WatchView> {
    get_watch_core(&context.state, &input.key).await
}

async fn patch_watch_core(
    st: &AppState,
    req: watches_operations::update::Input,
) -> ApiResult<WatchView> {
    let key = req.key.as_str();
    let o = require_watch(&st.db, key).await?;
    if o.agent_spec.is_some()
        && (req.program.is_some()
            || req.scope.is_some()
            || req.params.is_some()
            || req.capabilities.is_some()
            || req.model.is_some()
            || req.effort.is_some()
            || req.cooldown_secs.is_some()
            || req.profile.is_some())
    {
        return Err(AppError::bad_request("agent watches configure their task and profile through agent; script fields do not apply"));
    }

    if let Some(program) = &req.program {
        validate_program(program)?;
    }
    if let Some(caps) = &req.capabilities {
        validate_capabilities(caps)?;
    }
    if let Some(profile) = &req.profile {
        validate_watch_profile(&st.db, profile).await?;
    }

    // An explicit trigger wins; otherwise, when the program changes, re-evaluate
    // the new script's manifest (with the effective params) so subscriptions
    // follow the script — the same reconcile create does.
    let trigger_spec = match &req.trigger {
        Some(t) => Some(t.to_string()),
        None => match &req.program {
            Some(program) => {
                let params = req.params.clone().unwrap_or_else(|| o.params());
                let fallback =
                    program_default_trigger(program).unwrap_or_else(|| o.trigger_spec.clone());
                Some(reconcile_trigger(st, program, &params, &fallback).await)
            }
            None => None,
        },
    };
    let agent = req.agent.clone().or(o.agent()?);
    validate_agent_definition(
        st,
        trigger_spec.as_deref().unwrap_or(&o.trigger_spec),
        agent.as_ref(),
        req.late_grace_secs,
        req.run_timeout_secs,
    )
    .await?;
    let patch = watch_store::WatchUpdate {
        trigger_spec,
        scope: req.scope.map(|v| v.to_string()),
        program: req.program,
        params: req.params.map(|v| v.to_string()),
        capabilities: req.capabilities,
        profile: req
            .agent
            .as_ref()
            .map(|a| a.profile.clone())
            .or(req.profile),
        model: req.model,
        effort: req.effort,
        cooldown_secs: req.cooldown_secs,
        agent_spec: req.agent.as_ref().map(serde_json::to_string).transpose()?,
        misfire_policy: req.misfire_policy.map(|p| match p {
            weaver_core::schedule::MisfirePolicy::Skip => "skip".into(),
            _ => "coalesce".into(),
        }),
        late_grace_secs: req.late_grace_secs,
        run_timeout_secs: req.run_timeout_secs,
    };
    if !patch.is_empty() {
        watch_store::update(&st.db, &o.id, &patch).await?;
    }
    if let Some(enabled) = req.enabled {
        watch_store::set_enabled(&st.db, &o.id, enabled).await?;
        sqlx::query("UPDATE watches SET paused = ? WHERE id = ?")
            .bind(!enabled)
            .bind(&o.id)
            .execute(&st.db)
            .await?;
    }
    let o = require_watch(&st.db, &o.id).await?;
    watch_view(&st.db, &o).await
}

pub(super) async fn update_watch_operation(
    context: OperationContext,
    input: watches_operations::update::Input,
) -> ApiResult<WatchView> {
    patch_watch_core(&context.state, input).await
}

async fn delete_watch_core(st: &AppState, key: &str) -> ApiResult<WatchDeleteResult> {
    let o = require_watch(&st.db, key).await?;
    if weaver_core::occurrence::active(&st.db)
        .await?
        .iter()
        .any(|r| r.watch_id == o.id)
    {
        return Err(AppError::conflict(
            "disable the watch and wait for its active agent to stop before deleting",
        ));
    }
    watch_store::delete(&st.db, &o.id).await?;
    tracing::info!(watch = %o.id, name = %o.name, "watch deleted");
    Ok(WatchDeleteResult {
        deleted: true,
        id: o.id,
    })
}

pub(super) async fn delete_watch_operation(
    context: OperationContext,
    input: watches_operations::delete::Input,
) -> ApiResult<WatchDeleteResult> {
    delete_watch_core(&context.state, &input.key).await
}

/// Fire a round now, in the daemon (the single terminal owner), and report its
/// outcome. `dry_run` stubs every mutating action — the iteration primitive,
/// safe to repeat. Re-reads the closed run row to return outcome + summary.
async fn run_watch_core(st: &AppState, key: &str, dry_run: bool) -> ApiResult<WatchRunResult> {
    let o = require_watch(&st.db, key).await?;
    let reason = if dry_run { "run (dry)" } else { "run" };
    let run_id = ov_engine::fire_now(st, &o.id, dry_run, reason).await?;
    let run = watch_store::recent_runs(&st.db, &o.id, 50)
        .await?
        .into_iter()
        .find(|r| r.id == run_id);
    let (outcome, summary) = run
        .map(|r| (r.outcome, r.summary))
        .unwrap_or_else(|| (String::new(), String::new()));
    Ok(WatchRunResult {
        run_id,
        outcome,
        summary,
    })
}

/// `watches.run`. Not exercised here — this handler fires a watch round for
/// real when invoked, so it is wired but deliberately never called by any
/// test or manual check.
pub(super) async fn run_watch_operation(
    context: OperationContext,
    input: watches_operations::run::Input,
) -> ApiResult<WatchRunResult> {
    run_watch_core(&context.state, &input.key, input.dry_run).await
}

/// Run a one-shot ACP prompt and return its text — the judgement primitive
/// watch programs call. Best-effort by contract: an absent or failing runtime
/// returns `None` rather than an error, so callers degrade to their
/// deterministic fallback. Used by `agents.oneshot` and watch execution.
async fn agent_oneshot_core(
    st: &AppState,
    prompt: &str,
    profile_name: &str,
    agent_name: &str,
    model: &str,
    effort: &str,
) -> ApiResult<Option<String>> {
    if prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt must be non-empty"));
    }
    let budget = ov_engine::get_int(&st.db, "watch.default_timeout_secs", 600)
        .await
        .max(1) as u64;
    let profile = if profile_name.trim().is_empty() {
        None
    } else {
        Some(
            crate::profile::get(&st.db, profile_name.trim())
                .await?
                .ok_or_else(|| {
                    AppError::bad_request(format!("unknown profile '{}'", profile_name.trim()))
                })?,
        )
    };
    if let Some(profile) = &profile {
        if !profile.is_automation_safe() || profile.protocol != "acp" {
            return Err(AppError::bad_request(format!(
                "profile '{}' is not automation-safe ACP",
                profile.name
            )));
        }
    }
    let runtime = agent_name.trim();
    let runtime = profile
        .as_ref()
        .map(|profile| profile.agent_kind.as_str())
        .unwrap_or_else(|| {
            if runtime.is_empty() {
                "claude"
            } else {
                runtime
            }
        });
    Ok(agent::AgentManager::new(&st.db, &st.acp)
        .run_oneshot(
            runtime,
            prompt,
            model,
            effort,
            profile.as_ref(),
            std::time::Duration::from_secs(budget),
        )
        .await)
}

async fn agent_oneshot_operation(
    context: OperationContext,
    input: agents_operations::oneshot::Input,
) -> ApiResult<agents_operations::oneshot::Output> {
    let output = agent_oneshot_core(
        &context.state,
        &input.prompt,
        &input.profile,
        &input.agent,
        &input.model,
        &input.effort,
    )
    .await?;
    Ok(agents_operations::oneshot::Output { output })
}

async fn validate_watch_profile(db: &crate::Db, name: &str) -> ApiResult<()> {
    let name = name.trim();
    let profile = crate::profile::get(db, name)
        .await?
        .ok_or_else(|| AppError::bad_request(format!("unknown profile '{name}'")))?;
    if profile.retired || !profile.is_automation_safe() || profile.protocol != "acp" {
        return Err(AppError::bad_request(format!(
            "watch profile '{name}' must be automation-safe and use ACP"
        )));
    }
    Ok(())
}

async fn watch_runs_core(
    st: &AppState,
    key: &str,
    limit: Option<i64>,
) -> ApiResult<Vec<WatchRunView>> {
    let o = require_watch(&st.db, key).await?;
    let limit = limit.unwrap_or(50).clamp(1, 1000);
    let runs = watch_store::recent_runs(&st.db, &o.id, limit).await?;
    Ok(runs.into_iter().map(WatchRunView::from).collect())
}

pub(super) async fn watch_runs_operation(
    context: OperationContext,
    input: watches_operations::runs::Input,
) -> ApiResult<Vec<WatchRunView>> {
    watch_runs_core(&context.state, &input.key, input.limit).await
}

/// Serialize an optional structured-JSON field into the text column the model
/// stores, falling back to the model default when absent.
fn json_text(value: Option<Value>, default: &str) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| default.to_string())
}

async fn validate_agent_definition(
    st: &AppState,
    trigger: &str,
    agent: Option<&weaver_core::schedule::AgentTarget>,
    grace: Option<i64>,
    timeout: Option<i64>,
) -> ApiResult<()> {
    let trigger: watch_store::Trigger =
        serde_json::from_str(trigger).map_err(|e| AppError::bad_request(e.to_string()))?;
    weaver_core::schedule::validate(&trigger).map_err(|e| AppError::bad_request(e.to_string()))?;
    if trigger.is_scheduled() && agent.is_none() {
        return Err(AppError::bad_request(
            "cron and interval watches require an agent target",
        ));
    }
    if let Some(agent) = agent {
        agent
            .validate()
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        validate_watch_profile(&st.db, &agent.profile).await?;
    }
    if grace.is_some_and(|v| !(0..=86400).contains(&v))
        || timeout.is_some_and(|v| !(1..=86400).contains(&v))
    {
        return Err(AppError::bad_request(
            "late grace must be 0..86400 seconds and timeout 1..86400 seconds",
        ));
    }
    Ok(())
}

async fn preview_operation(
    _context: OperationContext,
    input: watches_operations::preview::Input,
) -> ApiResult<Vec<String>> {
    let trigger =
        serde_json::from_value(input.trigger).map_err(|e| AppError::bad_request(e.to_string()))?;
    let mut now = chrono::Utc::now();
    let mut times = Vec::new();
    for _ in 0..5 {
        let next = weaver_core::schedule::next(&trigger, now)
            .map_err(|e| AppError::bad_request(e.to_string()))?
            .ok_or_else(|| AppError::bad_request("a cron or interval is required"))?;
        times.push(weaver_core::schedule::iso(next));
        now = next;
    }
    Ok(times)
}
async fn occurrences_operation(
    context: OperationContext,
    input: watches_operations::occurrences::Input,
) -> ApiResult<Vec<weaver_core::occurrence::Occurrence>> {
    let watch = require_watch(&context.state.db, &input.key).await?;
    Ok(weaver_core::occurrence::history(
        &context.state.db,
        &watch.id,
        input.limit.unwrap_or(50).clamp(1, 1000),
    )
    .await?)
}
async fn state_operation(
    context: OperationContext,
    input: watches_operations::state::Input,
) -> ApiResult<Value> {
    let occurrence = super::scheduled::own_occurrence(&context.state, &input.branch).await?;
    let watch = require_watch(&context.state.db, &occurrence.watch_id).await?;
    if let Some(value) = input.value {
        let version = input.expected_version.ok_or_else(|| {
            AppError::bad_request("expected_version is required for state writes")
        })?;
        let version =
            weaver_core::occurrence::state(&context.state.db, &occurrence, &value, version)
                .await
                .map_err(|e| AppError::conflict(e.to_string()))?;
        return Ok(json!({"value": value, "version": version}));
    }
    Ok(json!({"value": watch.state(), "version": watch.state_version}))
}

pub(super) async fn reconcile_watch(
    st: &AppState,
    declared: watches_operations::create::Input,
) -> ApiResult<()> {
    let existing = watch_store::get_by_name(&st.db, declared.name.trim()).await?;
    let Some(existing) = existing else {
        let watch = create_watch_core(st, declared).await?;
        sqlx::query("UPDATE watches SET deployment_managed = 1 WHERE id = ?")
            .bind(watch.id)
            .execute(&st.db)
            .await?;
        return Ok(());
    };
    let trigger = declared.trigger.unwrap_or_else(|| json!({})).to_string();
    validate_agent_definition(
        st,
        &trigger,
        declared.agent.as_ref(),
        declared.late_grace_secs,
        declared.run_timeout_secs,
    )
    .await?;
    let agent = serde_json::to_string(&declared.agent)?;
    let grace = declared.late_grace_secs.unwrap_or(600);
    let timeout = declared.run_timeout_secs.unwrap_or(300);
    let policy = match declared.misfire_policy.unwrap_or_default() {
        weaver_core::schedule::MisfirePolicy::Skip => "skip",
        _ => "coalesce",
    };
    let enabled = declared.enabled.unwrap_or(false) && !existing.paused;
    if existing.trigger_spec != trigger
        || existing.agent_spec.as_deref() != Some(agent.as_str())
        || existing.late_grace_secs != grace
        || existing.run_timeout_secs != timeout
        || existing.misfire_policy != policy
    {
        watch_store::update(
            &st.db,
            &existing.id,
            &watch_store::WatchUpdate {
                trigger_spec: (existing.trigger_spec != trigger).then_some(trigger),
                agent_spec: Some(agent),
                profile: declared.agent.map(|a| a.profile),
                misfire_policy: Some(policy.into()),
                late_grace_secs: Some(grace),
                run_timeout_secs: Some(timeout),
                ..Default::default()
            },
        )
        .await?;
    }
    watch_store::set_enabled(&st.db, &existing.id, enabled).await?;
    Ok(())
}
