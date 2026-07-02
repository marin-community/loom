use std::path::PathBuf;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{
    AgentOneshotReq, CreateOverlookerReq, OverlookerRunView, OverlookerView, PatchOverlookerReq,
    ProgramView, RunOverlookerReq,
};
use weaver_core::overlooker::{self as ov, Overlooker};

use crate::agent;
use crate::db::Db;
use crate::overlooker as ov_engine;

use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Overlookers — the operator + authoring surface (server-owned state)
// ---------------------------------------------------------------------------

/// Build an [`OverlookerView`] for an overlooker, joining the most recent
/// round's outcome from the run history.
async fn overlooker_view(db: &Db, o: &Overlooker) -> ApiResult<OverlookerView> {
    let last_outcome = ov::recent_runs(db, &o.id, 1)
        .await?
        .into_iter()
        .next()
        .map(|r| r.outcome);
    Ok(OverlookerView::from_parts(o, last_outcome))
}

#[derive(Debug, Deserialize)]
pub(super) struct RunsQuery {
    /// How many recent rounds to return; defaults to 50.
    limit: Option<i64>,
}

/// Reject a capability set that isn't a subset of the known ladder, naming the
/// offender. Returns the cleaned set on success.
fn validate_capabilities(caps: &[String]) -> ApiResult<()> {
    for c in caps {
        if !ov::CAPABILITIES.contains(&c.as_str()) {
            return Err(AppError::bad_request(format!(
                "unknown capability '{c}' — expected a subset of {}",
                ov::CAPABILITIES.join(", ")
            )));
        }
    }
    Ok(())
}

/// A program reference must be a known `builtin:<name>` program or an absolute
/// path (a file under `~/.weaver/overlookers/`). An unknown builtin is rejected
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

/// `GET /api/overlookers/programs` — the builtin program registry: what the
/// create form offers and the panel's read-only script viewer renders.
pub(super) async fn list_programs() -> Json<Vec<ProgramView>> {
    Json(crate::builtins::BUILTINS.iter().map(|b| b.view()).collect())
}

/// Resolve an overlooker (by id or name) or 404.
async fn require_overlooker(db: &Db, key: &str) -> ApiResult<Overlooker> {
    ov::resolve(db, key)
        .await?
        .ok_or_else(|| AppError::not_found("overlooker"))
}

pub(super) async fn list_overlookers(
    State(st): State<AppState>,
) -> ApiResult<Json<Vec<OverlookerView>>> {
    let mut out = Vec::new();
    for o in ov::list(&st.db).await? {
        out.push(overlooker_view(&st.db, &o).await?);
    }
    Ok(Json(out))
}

pub(super) async fn create_overlooker(
    State(st): State<AppState>,
    Json(req): Json<CreateOverlookerReq>,
) -> ApiResult<Json<OverlookerView>> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("name must not be empty"));
    }
    if ov::get_by_name(&st.db, &name).await?.is_some() {
        return Err(AppError::conflict(format!(
            "an overlooker named '{name}' already exists"
        )));
    }

    let defaults = ov::NewOverlooker::default();
    let program = req.program.unwrap_or(defaults.program);
    validate_program(&program)?;
    let capabilities = req.capabilities.unwrap_or(defaults.capabilities);
    validate_capabilities(&capabilities)?;
    let params = json_text(req.params, &defaults.params);

    // The script declares what wakes it: evaluate its subscription manifest
    // (register mode) unless the caller pinned an explicit trigger.
    let trigger_spec = match req.trigger {
        Some(t) => t.to_string(),
        None => {
            let params_value = serde_json::from_str(&params).unwrap_or_else(|_| json!({}));
            let fallback = program_default_trigger(&program).unwrap_or(defaults.trigger_spec);
            reconcile_trigger(&st, &program, &params_value, &fallback).await
        }
    };

    let new = ov::NewOverlooker {
        name,
        trigger_spec,
        scope: json_text(req.scope, &defaults.scope),
        program,
        params,
        capabilities,
        model: req.model.unwrap_or(defaults.model),
        effort: req.effort.unwrap_or(defaults.effort),
        cooldown_secs: req.cooldown_secs.unwrap_or(defaults.cooldown_secs),
        enabled: req.enabled.unwrap_or(defaults.enabled),
    };
    let o = ov::create(&st.db, &new).await?;
    tracing::info!(overlooker = %o.id, name = %o.name, "overlooker created");
    Ok(Json(overlooker_view(&st.db, &o).await?))
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

pub(super) async fn get_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<OverlookerView>> {
    let o = require_overlooker(&st.db, &key).await?;
    Ok(Json(overlooker_view(&st.db, &o).await?))
}

pub(super) async fn patch_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PatchOverlookerReq>,
) -> ApiResult<Json<OverlookerView>> {
    let o = require_overlooker(&st.db, &key).await?;

    if let Some(program) = &req.program {
        validate_program(program)?;
    }
    if let Some(caps) = &req.capabilities {
        validate_capabilities(caps)?;
    }
    if let Some(enabled) = req.enabled {
        ov::set_enabled(&st.db, &o.id, enabled).await?;
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
                Some(reconcile_trigger(&st, program, &params, &fallback).await)
            }
            None => None,
        },
    };
    let patch = ov::OverlookerUpdate {
        trigger_spec,
        scope: req.scope.map(|v| v.to_string()),
        program: req.program,
        params: req.params.map(|v| v.to_string()),
        capabilities: req.capabilities,
        model: req.model,
        effort: req.effort,
        cooldown_secs: req.cooldown_secs,
    };
    if !patch.is_empty() {
        ov::update(&st.db, &o.id, &patch).await?;
    }
    let o = require_overlooker(&st.db, &o.id).await?;
    Ok(Json(overlooker_view(&st.db, &o).await?))
}

pub(super) async fn delete_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let o = require_overlooker(&st.db, &key).await?;
    ov::delete(&st.db, &o.id).await?;
    tracing::info!(overlooker = %o.id, name = %o.name, "overlooker deleted");
    Ok(Json(json!({ "deleted": true })))
}

/// Fire a round now, in the daemon (the single terminal owner), and report its
/// outcome. `dry_run` stubs every mutating action — the iteration primitive,
/// safe to repeat. Re-reads the closed run row to surface outcome + summary.
pub(super) async fn run_overlooker(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<RunOverlookerReq>,
) -> ApiResult<Json<Value>> {
    let o = require_overlooker(&st.db, &key).await?;
    let reason = if req.dry_run { "run (dry)" } else { "run" };
    let run_id = ov_engine::fire_now(&st, &o.id, req.dry_run, reason).await?;
    let run = ov::recent_runs(&st.db, &o.id, 50)
        .await?
        .into_iter()
        .find(|r| r.id == run_id);
    let (outcome, summary) = run
        .map(|r| (r.outcome, r.summary))
        .unwrap_or_else(|| (String::new(), String::new()));
    Ok(Json(json!({
        "run_id": run_id,
        "outcome": outcome,
        "summary": summary,
    })))
}

/// Run a one-shot headless agent and return `{output}` — the judgement
/// primitive overlooker programs call. The daemon owns the agent command
/// (`WEAVER_OVERLOOKER_AGENT_CMD`, default `claude -p`) and the timeout
/// budget. Best-effort by contract: an absent or failing agent returns
/// `{output: null}` rather than an error, so callers degrade to their
/// deterministic fallback.
pub(super) async fn agent_oneshot(
    State(st): State<AppState>,
    Json(req): Json<AgentOneshotReq>,
) -> ApiResult<Json<Value>> {
    if req.prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt must be non-empty"));
    }
    let budget = ov_engine::get_int(&st.db, "overlooker.default_timeout_secs", 600)
        .await
        .max(1) as u64;
    let output = agent::run_oneshot(
        &req.prompt,
        &req.model,
        &req.effort,
        std::time::Duration::from_secs(budget),
    )
    .await;
    Ok(Json(json!({ "output": output })))
}

pub(super) async fn overlooker_runs(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Vec<OverlookerRunView>>> {
    let o = require_overlooker(&st.db, &key).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 1000);
    let runs = ov::recent_runs(&st.db, &o.id, limit).await?;
    Ok(Json(
        runs.into_iter().map(OverlookerRunView::from).collect(),
    ))
}

/// Serialize an optional structured-JSON field into the text column the model
/// stores, falling back to the model default when absent.
fn json_text(value: Option<Value>, default: &str) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| default.to_string())
}
