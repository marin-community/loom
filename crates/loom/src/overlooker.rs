//! The Overlooker **engine** — the timer, dispatcher, and round executor that
//! run inside the loom daemon (the single owner of the tmux/session runtime).
//!
//! The storage + model (`Overlooker`, `Trigger`, `Scope`, the run audit) lives
//! in [`weaver_core::overlooker`]; this module is the live machinery that turns
//! those rows into action. It is built as **two halves of one event loop**, the
//! design of record in `docs/plans/overlooker.md`:
//!
//! * **The timer (producer).** For each enabled scheduled overlooker it keeps a
//!   `next_run_at`, and when due writes a `cron` system event into the same
//!   `events` stream session changes flow through — nothing more. A cron tick is
//!   a first-class, logged row.
//! * **The dispatcher (consumer).** A sibling of [`crate::monitor::run`] on its
//!   own independent watermark: it reads `events::since`, and for each new event
//!   fires every enabled overlooker whose trigger matches — a scheduled one
//!   matches its own `cron` tick, a reactive one matches a `tag` write (the
//!   tag's key/value become the trigger's match kind/level, so `attention` and
//!   `triage` tags still drive `{event:"attention"|"triage"}` triggers) or a
//!   `stale` tick, and `manual` ticks (operator "run now") fire the named
//!   overlooker.
//!
//! Both halves are folded into one [`run`] loop, self-gated on the
//! `overlooker.enabled` master switch so the daemon can always spawn it and it
//! idles cheaply when off.
//!
//! A **round** is one execution ([`fire`]). It is **level-triggered**: the event
//! that woke it is only a nudge to re-survey the *current* scoped fleet — it
//! never "handles" the specific event, so firing twice is idempotent. The round
//! runs a **program** under the non-optional guardrails — no-overlap, cooldown,
//! timeout, no-recursion — and records every mutating action as both an
//! `overlooker_runs` action entry and an `events` row (the audit rule). Two
//! program shapes share that one substrate ([`run_program`]): the builtin
//! **scripts** embedded from [`crate::builtins`] and custom program files —
//! both run by the same subprocess executor ([`run_script`]), reaching the
//! fleet only through the loom REST API.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::events;
use crate::session as session_mod;
use crate::web::AppState;
use weaver_core::branch as branch_mod;
use weaver_core::config as core_config;
use weaver_core::overlooker::{self as ov, Overlooker};

/// How often the engine wakes to drain new events and check the timer. Matches
/// the monitor's cadence closely enough that a reactive event is acted on
/// promptly without the loop being a busy spinner.
const TICK: Duration = Duration::from_millis(1500);

/// Read an integer setting, falling back to `default` on absence or parse
/// failure. `weaver_core::config` has bool/string getters but no int getter, so
/// the engine parses the raw value itself.
pub(crate) async fn get_int(db: &crate::Db, key: &str, default: i64) -> i64 {
    core_config::get(db, key)
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// The set of overlooker ids with a round currently in flight. Shared across the
/// dispatcher and any `fire_now` caller so the **no-overlap** guardrail holds no
/// matter what woke the round. A `Mutex<HashSet>` is enough: the critical
/// sections are tiny (insert/remove of one id) and rounds are not hot.
pub type InFlight = Arc<Mutex<HashSet<String>>>;

/// A fresh, empty in-flight set — one no-overlap domain. The engine loop holds
/// one for the lifetime of the daemon; an operator `fire_now` and tests each get
/// their own.
pub fn new_in_flight() -> InFlight {
    Arc::new(Mutex::new(HashSet::new()))
}

/// A round-scoped guard that removes its overlooker id from the in-flight set on
/// drop, so a panicking or early-returning round can never wedge the set and
/// block every future round of that overlooker.
struct InFlightGuard {
    set: InFlight,
    id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let set = self.set.clone();
        let id = self.id.clone();
        // Drop runs in a sync context; spawn the async removal. The set is only
        // read at the top of `fire`, so a brief delay before the id clears is
        // harmless (it just keeps a finished round "in flight" for an instant).
        tokio::spawn(async move {
            set.lock().await.remove(&id);
        });
    }
}

// ---------------------------------------------------------------------------
// The engine loop (timer + dispatcher)
// ---------------------------------------------------------------------------

/// The background engine: spawned in [`crate::server::serve`] alongside the
/// monitor and the GitHub poller. One loop drives both halves — the timer emits
/// `cron` ticks, then the dispatcher drains new events and fires matching
/// overlookers — so cron and reactive triggers share exactly one code path.
pub async fn run(state: AppState) {
    let in_flight = new_in_flight();
    // Independent watermark: process every event written after this id. Init
    // from the current max so a restart self-heals on the next tick rather than
    // replaying history (level-triggered — the round always re-surveys).
    let mut last_event = events::max_id(&state.db).await.unwrap_or(0);
    tracing::info!(
        tick_ms = TICK.as_millis() as u64,
        "overlooker engine started"
    );

    loop {
        tokio::time::sleep(TICK).await;

        // Master switch: when off, idle cheaply but keep the watermark current
        // so flipping it on doesn't replay the backlog accumulated while off.
        if !core_config::get_bool(
            &state.db,
            "overlooker.enabled",
            core_config::DEFAULT_OVERLOOKER_ENABLED,
        )
        .await
        {
            last_event = events::max_id(&state.db).await.unwrap_or(last_event);
            continue;
        }

        // 1. Timer (producer): emit `cron` ticks for any scheduled overlooker
        //    that is due. Each tick is a visible `events` row the dispatcher
        //    then consumes below.
        tick_timer(&state).await;

        // 2. Dispatcher (consumer): drain new events and fire matching rounds.
        match events::since(&state.db, last_event).await {
            Ok(new_events) => {
                for ev in new_events {
                    last_event = last_event.max(ev.id);
                    dispatch(&state, &in_flight, &ev).await;
                }
            }
            Err(e) => tracing::warn!("overlooker: reading new events failed: {e}"),
        }
    }
}

/// The timer half: for each enabled scheduled overlooker, compute (and persist)
/// its `next_run_at` if missing, and when it is due emit a `cron` system tick and
/// advance the schedule. Self-gating on the master switch happens in [`run`].
///
/// Public so a test can drive one timer pass without the loop's master-switch
/// gate or tick cadence; in production only [`run`] calls it.
pub async fn tick_timer(state: &AppState) {
    let overlookers = match ov::list_enabled(&state.db).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("overlooker timer: listing enabled failed: {e}");
            return;
        }
    };
    let now = Utc::now();
    for o in overlookers {
        let trigger = o.trigger();
        if !trigger.is_scheduled() {
            continue;
        }
        // Seed a never-scheduled overlooker's next-fire without firing it now.
        let next = match o.next_run_at.as_deref() {
            Some(ts) => parse_iso(ts),
            None => None,
        };
        let next = match next {
            Some(n) => n,
            None => {
                if let Some(n) = next_fire(&o, now) {
                    let _ = ov::set_schedule(&state.db, &o.id, None, Some(&iso(n))).await;
                }
                continue;
            }
        };
        if next > now {
            continue;
        }
        // Due: emit the cron tick (the dispatcher fires the round) and advance.
        if let Err(e) =
            events::record_system(&state.db, &state.bus, "cron", json!({ "overlooker": o.id }))
                .await
        {
            tracing::warn!(overlooker = %o.id, "overlooker timer: recording cron tick failed: {e}");
            continue;
        }
        let advanced = next_fire(&o, now).map(iso);
        let _ = ov::set_schedule(&state.db, &o.id, None, advanced.as_deref()).await;
    }
}

/// Route one new event to the overlookers it should fire.
///
/// * a `cron` system tick carries `{overlooker}` → fire that one (scheduled);
/// * a `manual` system tick carries `{overlooker, dry_run, reason}` → fire it
///   (operator "run now"), bypassing cooldown;
/// * any other event is a reactive nudge: for each enabled overlooker with a
///   matching reactive trigger, fire a (level-triggered) re-survey.
///
/// Public so a test (and the engine loop) can route a single event without the
/// full tick cadence; in production only [`run`] calls it.
pub async fn dispatch(state: &AppState, in_flight: &InFlight, ev: &events::Event) {
    match ev.kind.as_str() {
        "cron" => {
            if let Some(o) = resolve_target(state, ev).await {
                let _ = fire(state, in_flight, &o, "cron", false).await;
            }
        }
        "manual" => {
            if let Some(o) = resolve_target(state, ev).await {
                let dry_run = ev
                    .data
                    .get("dry_run")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let reason = ev
                    .data
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("manual");
                let _ = fire(state, in_flight, &o, reason, dry_run).await;
            }
        }
        // The engine's own audit rows (and a `cron`/`manual` already handled)
        // must never re-trigger a reactive round, or it would chase its tail.
        "overlooker" => {}
        ev_kind => {
            // Reactive: resolve the trigger's match `(kind, level, repo)`. A tag
            // write is one `"tag"` event carrying `{key, value}` — its key is the
            // match-kind and value the level, so a `{event:"attention",
            // level:"blocked"}` trigger fires off the `attention` tag event. Every
            // other reactive kind (`stale`, `pr_red`, hook-derived) matches on the
            // event kind with the level from `data.level`. The repo is the
            // repo_root of the event's branch; system rows (no branch) carry no
            // repo and match only repo-less triggers.
            let (match_kind, level, repo) = reactive_context(state, ev, ev_kind).await;
            let overlookers = match ov::list_enabled(&state.db).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!("overlooker dispatch: listing enabled failed: {e}");
                    return;
                }
            };
            for o in overlookers {
                if o.trigger()
                    .matches_event(&match_kind, level.as_deref(), repo.as_deref())
                {
                    let _ = fire(state, in_flight, &o, &format!("event:{match_kind}"), false).await;
                }
            }
        }
    }
}

/// The named overlooker carried by a `cron`/`manual` system tick's `{overlooker}`
/// field, if it still exists and is enabled.
async fn resolve_target(state: &AppState, ev: &events::Event) -> Option<Overlooker> {
    let key = ev.data.get("overlooker").and_then(Value::as_str)?;
    let o = ov::resolve(&state.db, key).await.ok().flatten()?;
    o.enabled.then_some(o)
}

/// The `(match_kind, level, repo)` an event presents for reactive matching.
///
/// * For a `"tag"` event the match-kind is the tag's `key` and the level its
///   `value` (an `attention` tag with value `blocked` matches a `{event:
///   "attention", level:"blocked"}` trigger). A cleared tag (empty value) yields
///   no level, matching only a level-agnostic `{event:"<key>"}` trigger.
/// * For every other reactive kind the match-kind is the event kind itself and
///   the level comes from `data.level`.
///
/// `repo` is the originating branch's `repo_root`; system events have no branch
/// and so no repo.
async fn reactive_context(
    state: &AppState,
    ev: &events::Event,
    ev_kind: &str,
) -> (String, Option<String>, Option<String>) {
    let (match_kind, level) = if ev_kind == "tag" {
        let key = ev
            .data
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let level = ev
            .data
            .get("value")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        (key, level)
    } else {
        let level = ev
            .data
            .get("level")
            .and_then(Value::as_str)
            .map(str::to_string);
        (ev_kind.to_string(), level)
    };
    if events::is_system(&ev.branch_id) {
        return (match_kind, level, None);
    }
    let repo = branch_mod::get(&state.db, &ev.branch_id)
        .await
        .ok()
        .flatten()
        .map(|b| b.repo_root);
    (match_kind, level, repo)
}

// ---------------------------------------------------------------------------
// The round executor + guardrails
// ---------------------------------------------------------------------------

/// Execute one round of `o`. The single code path for every trigger — cron,
/// manual, reactive. Returns the run id, or `None` when a guardrail skipped the
/// round before a run row was opened (no-overlap / cooldown).
///
/// Guardrails, in order:
/// 1. **no-overlap** — a re-fire while a round of the same overlooker is in
///    flight is dropped (no run row); the in-flight set is the gate.
/// 2. **cooldown** — a fire inside `max(cooldown_secs, default_cooldown_secs)`
///    of the last run is recorded `skipped`. A `manual`/`run-now` bypasses it.
/// 3. **timeout** — the program is wrapped in a wall-clock budget; an overrun
///    is recorded `error`.
/// 4. **no-recursion** — the survey ([`run_program`]) excludes overlooker warm
///    sessions, so a watcher never acts on another watcher.
pub async fn fire(
    state: &AppState,
    in_flight: &InFlight,
    o: &Overlooker,
    trigger_reason: &str,
    dry_run: bool,
) -> Option<i64> {
    // 1. No-overlap: claim the in-flight slot or drop silently. A dropped round
    //    is intentionally not a run row — it never started.
    {
        let mut set = in_flight.lock().await;
        if !set.insert(o.id.clone()) {
            tracing::debug!(overlooker = %o.id, "overlooker: round already in flight; skipping re-fire");
            return None;
        }
    }
    let _guard = InFlightGuard {
        set: in_flight.clone(),
        id: o.id.clone(),
    };

    let manual = trigger_reason == "manual" || trigger_reason.starts_with("run");
    let now = Utc::now();

    // 2. Cooldown: a non-manual re-fire inside the gap is recorded `skipped`.
    let cooldown = o
        .cooldown_secs
        .max(get_int(&state.db, "overlooker.default_cooldown_secs", 0).await);
    if !manual && cooldown > 0 {
        if let Some(last) = o.last_run_at.as_deref().and_then(parse_iso) {
            if (now - last).num_seconds() < cooldown {
                return record_skipped(
                    state,
                    o,
                    trigger_reason,
                    &format!("cooldown: {cooldown}s gap not elapsed"),
                )
                .await;
            }
        }
    }

    // Open the run row; everything from here closes it via `finish_run`.
    let run_id = match ov::start_run(&state.db, &o.id, trigger_reason).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(overlooker = %o.id, "overlooker: opening run row failed: {e}");
            return None;
        }
    };

    // 3. Timeout: budget the program run. An overrun is `error`, the schedule
    //    still advances so the next trigger fires.
    let timeout_secs = get_int(&state.db, "overlooker.default_timeout_secs", 600)
        .await
        .max(1);
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs as u64),
        run_program(state, o, dry_run),
    )
    .await;

    let (outcome, summary, actions) = match result {
        Ok(Ok(r)) => (r.outcome, r.summary, r.actions),
        Ok(Err(e)) => (
            "error".to_string(),
            format!("round failed: {e}"),
            Value::Array(vec![]),
        ),
        Err(_) => (
            "error".to_string(),
            format!("round exceeded {timeout_secs}s budget"),
            Value::Array(vec![]),
        ),
    };

    let _ = ov::finish_run(&state.db, run_id, &outcome, &summary, &actions).await;
    // Stamp the schedule: last_run_at = now; advance next_run_at for a scheduled
    // overlooker (a reactive one keeps None).
    let next = next_fire(o, now).map(iso);
    let _ = ov::set_schedule(&state.db, &o.id, Some(&iso(now)), next.as_deref()).await;

    Some(run_id)
}

/// Record a `skipped` round (cooldown / a guardrail that still merits an audit
/// row, unlike the silent no-overlap drop). Returns the run id.
async fn record_skipped(
    state: &AppState,
    o: &Overlooker,
    trigger_reason: &str,
    summary: &str,
) -> Option<i64> {
    let run_id = ov::start_run(&state.db, &o.id, trigger_reason).await.ok()?;
    let _ = ov::finish_run(&state.db, run_id, "skipped", summary, &Value::Array(vec![])).await;
    Some(run_id)
}

// ---------------------------------------------------------------------------
// The program substrate — the subprocess script executor
// ---------------------------------------------------------------------------

/// The result of running a program: an outcome, a one-line human summary, and the
/// JSON array of actions for the run's audit trail.
struct RoundResult {
    outcome: String,
    summary: String,
    actions: Value,
}

/// Dispatch to the program the overlooker names: a builtin **script**
/// embedded from [`crate::builtins`], or a custom program file (an absolute
/// path, conventionally under `~/.weaver/overlookers/`) — both run on
/// [`run_script`].
async fn run_program(
    state: &AppState,
    o: &Overlooker,
    dry_run: bool,
) -> anyhow::Result<RoundResult> {
    if let Some(source) = crate::builtins::find(&o.program).map(|b| b.source) {
        let file_name = program_file_name(&o.program);
        return run_script(
            state,
            o,
            dry_run,
            ScriptSource::Embedded { file_name, source },
        )
        .await;
    }
    let path = std::path::PathBuf::from(&o.program);
    if path.is_absolute() {
        return run_script(state, o, dry_run, ScriptSource::File(path)).await;
    }
    Err(anyhow::anyhow!(
        "unknown overlooker program '{}' — expected 'builtin:<name>' or an absolute path",
        o.program
    ))
}

/// A scratch file name for an embedded builtin: `builtin:pr-label` →
/// `pr-label.py`, so its tracebacks read like the program they came from.
fn program_file_name(program: &str) -> String {
    format!("{}.py", program.strip_prefix("builtin:").unwrap_or(program))
}

/// Where a script program's code comes from: embedded in the binary (a
/// builtin) or a file on disk (a custom program).
enum ScriptSource {
    Embedded {
        file_name: String,
        source: &'static str,
    },
    File(std::path::PathBuf),
}

/// Whether a script opts into PEP 723 inline metadata (`# /// script`). Such a
/// script declares its own dependencies, so the engine prefers `uv run
/// --script` — which resolves them — when `uv` is installed; a plain script
/// runs under `python3` directly.
fn has_pep723(source: &str) -> bool {
    source.lines().any(|l| l.trim() == "# /// script")
}

/// Whether the `uv` CLI is usable. Probed once and cached, like
/// [`crate::github::gh_available`] — absence is normal and shouldn't cost a
/// process spawn every round.
async fn uv_available() -> bool {
    static AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *AVAILABLE
        .get_or_init(|| async {
            tokio::process::Command::new("uv")
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .await
}

/// Run a **script program**: an env-stripped subprocess (the lint-review
/// precedent, like [`crate::agent::run_oneshot`]) that reaches the fleet only
/// through the loom REST API. `$WEAVER_API` carries the daemon's own address
/// and `$WEAVER_OVERLOOKER` the round config (`{id, name, program, params,
/// scope, capabilities, model, effort, dry_run}`); the vendored `weaver_loom`
/// module rides
/// `PYTHONPATH` so every program can import the API layer with no install
/// step. The contract is to print one JSON object — `{outcome, summary,
/// actions}` — as the final stdout line; a non-zero exit or unparseable
/// stdout errors the round. The wall-clock budget in [`fire`] bounds it, and
/// `kill_on_drop` reaps the subprocess when that budget cancels the future.
///
/// The interpreter is `python3`, or `uv run --script` when the script declares
/// PEP 723 inline metadata and `uv` is installed (so a custom program can
/// declare third-party dependencies; the builtins are stdlib-only).
async fn run_script(
    state: &AppState,
    o: &Overlooker,
    dry_run: bool,
    src: ScriptSource,
) -> anyhow::Result<RoundResult> {
    // One scratch dir per round: the vendored module always lands here (for
    // PYTHONPATH), an embedded builtin's source too (so a traceback carries a
    // real file/line). Removed when the round ends.
    let scratch = tempfile::tempdir().map_err(|e| anyhow::anyhow!("creating scratch dir: {e}"))?;
    let module = scratch.path().join("weaver_loom.py");
    tokio::fs::write(&module, crate::builtins::PYTHON_MODULE)
        .await
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", module.display()))?;

    let (script_path, source) = match &src {
        ScriptSource::Embedded { file_name, source } => {
            let path = scratch.path().join(file_name);
            tokio::fs::write(&path, source)
                .await
                .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
            (path, source.to_string())
        }
        // The source is read only to detect PEP 723 metadata; a missing file
        // surfaces as the spawn error below, with the path in it.
        ScriptSource::File(path) => (
            path.clone(),
            tokio::fs::read_to_string(path).await.unwrap_or_default(),
        ),
    };

    let pythonpath = match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            format!("{}:{existing}", scratch.path().display())
        }
        _ => scratch.path().display().to_string(),
    };

    let config = json!({
        "id": o.id,
        "name": o.name,
        "program": o.program,
        "params": o.params(),
        "scope": serde_json::to_value(o.scope()).unwrap_or(Value::Null),
        "capabilities": o.capabilities(),
        "model": o.model,
        "effort": o.effort,
        "dry_run": dry_run,
    });

    let interpreter = if has_pep723(&source) && uv_available().await {
        "uv"
    } else {
        "python3"
    };
    let mut command = tokio::process::Command::new(interpreter);
    if interpreter == "uv" {
        command.args(["run", "--quiet", "--script"]);
    }
    command
        .arg(&script_path)
        .env("WEAVER_API", api_base(&state.addr))
        .env("WEAVER_OVERLOOKER", config.to_string())
        .env("PYTHONPATH", pythonpath)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // The script reaches the daemon over the REST API; hand it the machine-local
    // token so it authenticates even when loopback trust is off.
    if let Some(token) = crate::agent::read_local_token() {
        command.env("LOOM_TOKEN", token);
    }
    for key in crate::agent::STRIPPED_ENV {
        command.env_remove(key);
    }

    let out = command.output().await.map_err(|e| {
        anyhow::anyhow!(
            "spawning {interpreter} for '{}' failed: {e} (is {interpreter} installed?)",
            o.program
        )
    })?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        anyhow::bail!(
            "script exited with {}: {}",
            out.status.code().unwrap_or(-1),
            tail(&stderr, 400)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_round_result(&stdout).ok_or_else(|| {
        anyhow::anyhow!(
            "script printed no result JSON object ({{outcome, summary, actions}}); stdout: {}; stderr: {}",
            tail(&stdout, 200),
            tail(&stderr, 200)
        )
    })
}

/// The REST base URL a script subprocess targets — the daemon's own bound
/// address. A wildcard bind (`0.0.0.0` / `[::]`) is mapped to loopback, since
/// "every interface" is not a dialable host.
fn api_base(addr: &str) -> String {
    let dialable = addr
        .strip_prefix("0.0.0.0:")
        .or_else(|| addr.strip_prefix("[::]:"))
        .map(|port| format!("127.0.0.1:{port}"))
        .unwrap_or_else(|| addr.to_string());
    format!("http://{dialable}")
}

/// Parse a script's stdout into a [`RoundResult`]: the whole stdout as one
/// JSON object, or — so a script may log progress lines first — the **last**
/// stdout line that is one. The fields are read leniently: a missing/unknown
/// `outcome` reads as `ok`, a missing `summary` as empty, a missing/non-array
/// `actions` as none — so a minimal script stays minimal.
fn parse_round_result(stdout: &str) -> Option<RoundResult> {
    let trimmed = stdout.trim();
    parse_result_object(trimmed).or_else(|| trimmed.lines().rev().find_map(parse_result_object))
}

/// One candidate result line → [`RoundResult`], if it is a JSON object.
fn parse_result_object(text: &str) -> Option<RoundResult> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let obj = v.as_object()?;
    let outcome = obj
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|o| ov::OUTCOMES.contains(o))
        .unwrap_or("ok");
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let actions = obj
        .get("actions")
        .filter(|a| a.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    Some(RoundResult {
        outcome: outcome.to_string(),
        summary: summary.to_string(),
        actions,
    })
}

/// The last `n` bytes-ish of a process stream (on a char boundary), so an
/// error summary carries the end of a traceback without unbounded length.
fn tail(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    let start = s
        .char_indices()
        .rev()
        .nth(n - 1)
        .map(|(i, _)| i)
        .unwrap_or(0);
    format!("…{}", &s[start..])
}

// ---------------------------------------------------------------------------
// The in-process entry point for the operator CLI (T8 will call this)
// ---------------------------------------------------------------------------

/// Fire `key` (an overlooker id or name) now, returning the run id. Used by the
/// operator's `loom overlooker run` (a later task) and by tests to drive a round
/// without waiting on the timer.
///
/// It **runs the round directly** rather than injecting a `manual` event: the
/// caller gets the run id synchronously and a result it can inspect, the round
/// bypasses cooldown (a deliberate "run now"), and there is no dependence on the
/// dispatcher's tick cadence. The no-overlap and timeout guardrails still apply.
/// An injected `manual` event would also work (the dispatcher handles it) but
/// would be fire-and-forget; the direct path is cleaner for an operator command.
pub async fn fire_now(
    state: &AppState,
    key: &str,
    dry_run: bool,
    reason: &str,
) -> anyhow::Result<i64> {
    let o = ov::resolve(&state.db, key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no overlooker '{key}'"))?;
    // A fresh, request-scoped in-flight set: a direct operator run is its own
    // no-overlap domain (the engine loop guards its own concurrent fires).
    let in_flight = new_in_flight();
    let reason = if reason.is_empty() { "manual" } else { reason };
    fire(state, &in_flight, &o, reason, dry_run)
        .await
        .ok_or_else(|| anyhow::anyhow!("round skipped before it could open a run row"))
}

// ---------------------------------------------------------------------------
// Warm-session lifecycle (T12)
// ---------------------------------------------------------------------------

/// Ensure the warm overlooker `o` has its long-lived, engine-managed session,
/// returning its id. **Idempotent and reuse-first**: if `o` already owns a live
/// managed session, that id is returned (and re-linked into `warm_session_id` if
/// it had drifted) — no duplicate is spawned. The session id is stable across
/// rounds and across a daemon restart, which is what gives the overlooker its
/// across-round memory.
///
/// On first need it forks a dedicated worktree and brings up a real tmux session
/// (via [`crate::web::create_warm_session`], the same launch machinery ordinary
/// sessions use), stamps it `managed_by = o.id` so the fleet hides it, and
/// records its id on the overlooker.
///
/// A non-warm overlooker (`params.warm` unset) returns `Ok(None)` without
/// spawning anything. The repo to anchor the worktree is the overlooker's
/// `scope.repo`, else the most recently used repo; an overlooker with no repo to
/// anchor errors rather than guessing.
pub async fn ensure_warm_session(
    state: &AppState,
    o: &Overlooker,
) -> anyhow::Result<Option<String>> {
    if !o.warm() {
        return Ok(None);
    }

    // Reuse-first: an existing live managed session is the warm session. Keep its
    // id and (cheaply) repair the overlooker linkage if it drifted.
    if let Some(existing) = session_mod::active_managed_by(&state.db, &o.id).await? {
        if o.warm_session_id.as_deref() != Some(existing.id.as_str()) {
            ov::set_warm_session(&state.db, &o.id, Some(&existing.id)).await?;
        }
        return Ok(Some(existing.id));
    }

    // First need: anchor a worktree in the scoped repo, else the most-recent one.
    let repo_root = match o.scope().repo {
        Some(r) => std::path::PathBuf::from(r),
        None => {
            let recent = crate::repo::recent(&state.db, 1).await?;
            let r = recent.into_iter().next().ok_or_else(|| {
                anyhow::anyhow!("no repo to anchor a warm session for '{}'", o.name)
            })?;
            std::path::PathBuf::from(r.repo_root)
        }
    };

    let session = crate::web::create_warm_session(state, o, &repo_root)
        .await
        .map_err(|e| anyhow::anyhow!("creating warm session: {}", e.message()))?;
    ov::set_warm_session(&state.db, &o.id, Some(&session.id)).await?;
    Ok(Some(session.id))
}

// ---------------------------------------------------------------------------
// Schedule arithmetic (cron + `every` sugar)
// ---------------------------------------------------------------------------

/// The next fire time for a scheduled overlooker after `from`. A `cron` field is
/// parsed with `croner` (standard 5-field crontab); an `every` field is the
/// duration sugar (`30m`, `2h`, `45s`). A reactive (non-scheduled) overlooker
/// has no next fire.
fn next_fire(o: &Overlooker, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let trigger = o.trigger();
    if let Some(cron) = trigger.cron.as_deref() {
        return next_cron(cron, from);
    }
    if let Some(every) = trigger.every.as_deref() {
        return parse_every(every).map(|d| from + d);
    }
    None
}

/// Next occurrence of a crontab expression strictly after `from`, or `None` if
/// the expression doesn't parse (a bad cron never schedules rather than erroring
/// every tick).
fn next_cron(expr: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use std::str::FromStr;
    let cron = croner::Cron::from_str(expr).ok()?;
    cron.find_next_occurrence(&from, false).ok()
}

/// Parse the `every` duration sugar — a number with an `s`/`m`/`h` suffix
/// (`30m`, `2h`, `45s`). No new dependency; the engine parses it itself.
fn parse_every(spec: &str) -> Option<chrono::Duration> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(spec.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    match unit.trim() {
        "s" | "sec" | "secs" => Some(chrono::Duration::seconds(n)),
        "m" | "min" | "mins" => Some(chrono::Duration::minutes(n)),
        "h" | "hr" | "hrs" => Some(chrono::Duration::hours(n)),
        _ => None,
    }
}

/// Parse an ISO-8601 timestamp (the [`weaver_core::db::now_iso`] format) to UTC.
fn parse_iso(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Format a UTC time as the ISO-8601 string the rest of weaver stores.
fn iso(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_every_handles_s_m_h() {
        assert_eq!(parse_every("30m"), Some(chrono::Duration::minutes(30)));
        assert_eq!(parse_every("2h"), Some(chrono::Duration::hours(2)));
        assert_eq!(parse_every("45s"), Some(chrono::Duration::seconds(45)));
        assert_eq!(parse_every("nonsense"), None);
        assert_eq!(parse_every(""), None);
    }

    #[test]
    fn next_cron_advances_to_the_future() {
        let from = parse_iso("2026-06-08T10:30:00.000Z").unwrap();
        // Every hour on the hour → next is 11:00.
        let next = next_cron("0 * * * *", from).unwrap();
        assert_eq!(iso(next), "2026-06-08T11:00:00.000Z");
        // A malformed expression never schedules.
        assert!(next_cron("not a cron", from).is_none());
    }

    #[test]
    fn parse_round_result_is_lenient_but_requires_an_object() {
        // The full contract round-trips.
        let r = parse_round_result(
            r#"{"outcome":"noop","summary":"all calm","actions":[{"would":"mark"}]}"#,
        )
        .unwrap();
        assert_eq!(r.outcome, "noop");
        assert_eq!(r.summary, "all calm");
        assert_eq!(r.actions.as_array().unwrap().len(), 1);

        // A minimal object gets the lenient defaults; an unknown outcome
        // clamps to `ok` rather than inventing a new state.
        let r = parse_round_result(r#"{"outcome":"sideways"}"#).unwrap();
        assert_eq!(r.outcome, "ok");
        assert_eq!(r.summary, "");
        assert_eq!(r.actions, Value::Array(vec![]));

        // A script may log progress lines first; the result is the last JSON
        // object line on stdout.
        let r = parse_round_result(
            "surveying...\n3 sessions seen\n{\"outcome\":\"ok\",\"summary\":\"done\"}\n",
        )
        .unwrap();
        assert_eq!(r.summary, "done");

        // Anything without a JSON object is a contract violation.
        assert!(parse_round_result("").is_none());
        assert!(parse_round_result("not json").is_none());
        assert!(parse_round_result("[1, 2]").is_none());
    }

    #[test]
    fn has_pep723_detects_the_inline_metadata_block() {
        assert!(has_pep723(
            "# /// script\n# dependencies = []\n# ///\nprint()"
        ));
        assert!(has_pep723("#!/usr/bin/env python3\n# /// script\n# ///\n"));
        assert!(!has_pep723("import weaver_loom\n"));
        assert!(!has_pep723("# script\n# ///\n"));
    }

    #[test]
    fn api_base_maps_wildcard_binds_to_loopback() {
        assert_eq!(api_base("127.0.0.1:7878"), "http://127.0.0.1:7878");
        assert_eq!(api_base("0.0.0.0:7878"), "http://127.0.0.1:7878");
        assert_eq!(api_base("[::]:7878"), "http://127.0.0.1:7878");
    }

    #[test]
    fn tail_keeps_the_end_of_long_streams() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("a long traceback line", 4), "…line");
        // Multi-byte chars stay on a boundary.
        assert_eq!(tail("héllo wörld", 4), "…örld");
    }

    use crate::builtins::python3_available;

    /// An `AppState` over a fresh in-memory db plus an overlooker registered on
    /// `program` — the minimum for a [`fire`] round to run a script end to end.
    async fn script_fixture(program: &str) -> (AppState, Overlooker) {
        let state = AppState {
            db: crate::db::connect_in_memory().await.unwrap(),
            bus: events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
        };
        let o = ov::create(
            &state.db,
            &ov::NewOverlooker {
                name: "script-test".to_string(),
                program: program.to_string(),
                params: r#"{"label":"weaver"}"#.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        (state, o)
    }

    /// A custom script file round-trips the whole contract **through the
    /// vendored `weaver_loom` module**: the engine puts the API layer on
    /// PYTHONPATH, the `Round` context exposes the `$WEAVER_OVERLOOKER` config
    /// (params and the dry-run flag included), and `finish()`'s printed
    /// `{outcome, summary, actions}` lands on the run row.
    #[tokio::test]
    async fn run_script_round_trips_the_contract() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("echo_config.py");
        std::fs::write(
            &script,
            r#"
import json
from weaver_loom import Round
rnd = Round()
rnd.would("label", note="from the script")
rnd.finish("name=%s label=%s dry=%s api=%s" % (
    rnd.name, rnd.params["label"], json.dumps(rnd.dry_run), rnd.client.base))
"#,
        )
        .unwrap();

        let (state, o) = script_fixture(&script.display().to_string()).await;
        let run_id = fire(&state, &new_in_flight(), &o, "manual", true)
            .await
            .unwrap();
        let run = ov::recent_runs(&state.db, &o.id, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == run_id)
            .unwrap();
        assert_eq!(run.outcome, "ok", "summary: {}", run.summary);
        assert!(
            run.summary
                .contains("name=script-test label=weaver dry=true api=http://127.0.0.1:0"),
            "the script saw its config through the module: {}",
            run.summary
        );
        let actions: Value = serde_json::from_str(&run.actions).unwrap();
        assert_eq!(actions[0]["would"], "label");
    }

    /// A failing script errors the round with the stderr tail in the summary,
    /// and a missing program file errors rather than wedging.
    #[tokio::test]
    async fn run_script_failures_are_recorded_as_errors() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("boom.py");
        std::fs::write(&script, "raise RuntimeError('kaboom')\n").unwrap();

        let (state, o) = script_fixture(&script.display().to_string()).await;
        let run_id = fire(&state, &new_in_flight(), &o, "manual", false)
            .await
            .unwrap();
        let run = ov::recent_runs(&state.db, &o.id, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == run_id)
            .unwrap();
        assert_eq!(run.outcome, "error");
        assert!(
            run.summary.contains("kaboom"),
            "the stderr tail names the failure: {}",
            run.summary
        );
    }
}
