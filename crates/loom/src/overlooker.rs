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
//! runs a **program** (the declarative `builtin:status` stock program here)
//! under the non-optional guardrails — no-overlap, cooldown, timeout,
//! no-recursion — and records every mutating action as both an
//! `overlooker_runs` action entry and an `events` row (the audit rule).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::session::{self as session_mod, Session};
use crate::web::AppState;
use crate::{events, tmux};
use weaver_core::branch::{self as branch_mod, Branch};
use weaver_core::config as core_config;
use weaver_core::overlooker::{self as ov, Overlooker};
use weaver_core::tags::{self, TRIAGE_KEY};

/// How often the engine wakes to drain new events and check the timer. Matches
/// the monitor's cadence closely enough that a reactive event is acted on
/// promptly without the loop being a busy spinner.
const TICK: Duration = Duration::from_millis(1500);

/// Read an integer setting, falling back to `default` on absence or parse
/// failure. `weaver_core::config` has bool/string getters but no int getter, so
/// the engine parses the raw value itself.
async fn get_int(db: &crate::Db, key: &str, default: i64) -> i64 {
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
// The program substrate (T5) + the stock `builtin:status` program (T11)
// ---------------------------------------------------------------------------

/// The result of running a program: an outcome, a one-line human summary, and the
/// JSON array of actions for the run's audit trail.
struct RoundResult {
    outcome: String,
    summary: String,
    actions: Value,
}

/// Dispatch to the program the overlooker names. Today only the declarative
/// `builtin:status` stock program exists; a custom file path would route to a
/// Python-subprocess executor (a follow-up, see the module docs).
async fn run_program(
    state: &AppState,
    o: &Overlooker,
    dry_run: bool,
) -> anyhow::Result<RoundResult> {
    match o.program.as_str() {
        "builtin:status" => builtin_status(state, o, dry_run).await,
        other => Err(anyhow::anyhow!(
            "unknown overlooker program '{other}' (only builtin:status is implemented)"
        )),
    }
}

/// The set of every overlooker's warm session id — the sessions a round must
/// never survey or act on (no-recursion: watchers don't watch watchers). The
/// survey already reads the *visible* fleet (managed sessions excluded), so this
/// is the secondary guard for a warm session referenced by `warm_session_id`
/// that is not yet marked `managed_by`.
async fn warm_session_ids(state: &AppState) -> HashSet<String> {
    ov::list(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| o.warm_session_id)
        .collect()
}

/// The declarative stock program: survey the scoped, non-terminal fleet and
/// stamp a triage mark on each in-scope session.
///
/// The judgement is best-effort-LLM with a deterministic fallback, so the engine
/// works fully without a real `claude`:
/// * when a prompt is configured *and* `claude` is on PATH, ask it for a level +
///   note from the session's screen preview (parsed leniently);
/// * otherwise — or on any parse/skip — fall back to a **rule** that mirrors the
///   session's own `attention` as the mark, with a derived note.
///
/// `dry_run` performs no mutation: each would-be mark is logged as a
/// `{would:"mark", …}` action instead, so an operator can repeat a dry round
/// safely.
async fn builtin_status(
    state: &AppState,
    o: &Overlooker,
    dry_run: bool,
) -> anyhow::Result<RoundResult> {
    let scope = o.scope();
    let warm = warm_session_ids(state).await;
    let can_mark = o.has_capability("mark");
    let can_nudge = o.has_capability("nudge");

    // The survey scope is the *visible* fleet: engine-managed (warm) sessions
    // are excluded at the source, so no round ever surveys a watcher's own
    // session (the no-recursion guarantee, enforced for every overlooker, not
    // just the one that owns the session). `warm_session_ids` below is a belt-
    // and-braces check for any warm session not yet marked `managed_by`.
    let sessions = session_mod::list_visible(&state.db).await?;
    let mut actions: Vec<Value> = Vec::new();
    let mut surveyed = 0usize;
    let mut marked = 0usize;
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    for session in sessions {
        // no-recursion: never survey an overlooker's own warm session.
        if warm.contains(&session.id) {
            continue;
        }
        if session_mod::is_terminal(&session.status) {
            continue;
        }
        let Some(branch) = branch_mod::get(&state.db, &session.branch_id).await? else {
            continue;
        };
        // The scope's attention filter matches the resolved `attention` tag
        // value; an absent tag is the calm `ok` state.
        let attention = attention_value(state, &branch).await;
        if !scope.admits(&attention, &branch.repo_root) {
            continue;
        }
        surveyed += 1;

        let (level, note) = judge(state, o, &session, &branch).await;
        // A non-storable level (an `ok`/clear decision) returns the triage axis to
        // calm rather than marking: clear the tag and log a `tag` event with an
        // empty value (the audit rule — every mutating action is also an events
        // row). The dry-run path mutates nothing.
        if !tags::is_valid_value(TRIAGE_KEY, &level) {
            if can_mark && !dry_run {
                tags::clear(&state.db, &branch.id, TRIAGE_KEY).await?;
                events::record_tag(
                    &state.db, &state.bus, &branch.id, TRIAGE_KEY, "", "", &o.name,
                )
                .await
                .ok();
            }
            continue;
        }

        if !can_mark {
            actions.push(json!({
                "session": session.id,
                "action": "observe",
                "level": level,
                "note": note,
            }));
            continue;
        }

        if dry_run {
            actions.push(json!({
                "session": session.id,
                "would": "mark",
                "level": level,
                "note": note,
            }));
            marked += 1;
            *counts.entry(level.clone()).or_default() += 1;
            continue;
        }

        // Apply the mark: write the `triage` tag and record the `tag` event so
        // the fleet badge updates and the monitor re-broadcasts it (the audit
        // rule — every mutating action is also an events row).
        tags::set(&state.db, &branch.id, TRIAGE_KEY, &level, &note, &o.name).await?;
        events::record_tag(
            &state.db, &state.bus, &branch.id, TRIAGE_KEY, &level, &note, &o.name,
        )
        .await
        .ok();
        actions.push(json!({
            "session": session.id,
            "action": "mark",
            "level": level,
            "note": note,
        }));
        marked += 1;
        *counts.entry(level.clone()).or_default() += 1;

        // The nudge rung of the intervention ladder: only when capability-granted
        // and the mark is not `ok`. Best-effort; a missing tmux just no-ops.
        if can_nudge && level != "ok" && tmux::has_session(&session.tmux_session).await {
            let text = format!("[overlooker {}] {note}", o.name);
            if dry_run {
                actions.push(json!({ "session": session.id, "would": "nudge", "text": text }));
            } else {
                let _ = tmux::send_literal(&session.tmux_session, &text).await;
                let _ = tmux::send_enter(&session.tmux_session).await;
                events::record(
                    &state.db,
                    &state.bus,
                    &branch.id,
                    "nudge",
                    json!({ "by": o.name, "text": text }),
                )
                .await
                .ok();
                actions.push(json!({ "session": session.id, "action": "nudge", "text": text }));
            }
        }
    }

    if surveyed == 0 {
        return Ok(RoundResult {
            outcome: "noop".to_string(),
            summary: "surveyed 0 sessions in scope".to_string(),
            actions: Value::Array(actions),
        });
    }

    let breakdown = counts
        .iter()
        .map(|(level, n)| format!("{n} {level}"))
        .collect::<Vec<_>>()
        .join(", ");
    let dry = if dry_run {
        " (dry run, no marks applied)"
    } else {
        ""
    };
    let verb = if dry_run { "would mark" } else { "marked" };
    let summary = if breakdown.is_empty() {
        format!("surveyed {surveyed}, {verb} 0{dry}")
    } else {
        format!("surveyed {surveyed}, {verb} {marked} ({breakdown}){dry}")
    };

    Ok(RoundResult {
        outcome: "ok".to_string(),
        summary,
        actions: Value::Array(actions),
    })
}

/// Decide a `(level, note)` for one session. Best-effort LLM judgement when a
/// prompt is configured and `claude` is reachable; otherwise the deterministic
/// rule — mirror the agent's own `attention` as the mark with a derived note —
/// so a round always produces a testable result without a real agent.
async fn judge(
    state: &AppState,
    o: &Overlooker,
    session: &Session,
    branch: &Branch,
) -> (String, String) {
    let prompt = o
        .params()
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();

    if !prompt.is_empty() {
        let screen = tmux::capture(&session.tmux_session, 200)
            .await
            .unwrap_or_default();
        let full = format!("{prompt}\n\nSession screen:\n{screen}\n");
        if let Some(out) = run_agent(state, o, &full).await {
            if let Some(judged) = parse_judgement(&out) {
                return judged;
            }
        }
    }

    // Rule fallback: mirror the agent's self-reported attention as the mark. The
    // agent's `attention` tag value (absent ⇒ calm `ok`) is the candidate level;
    // a non-storable value (`ok`) makes the caller clear the triage tag instead.
    let attention = attention_value(state, branch).await;
    let level = if tags::is_valid_value(TRIAGE_KEY, &attention) {
        attention.clone()
    } else {
        "ok".to_string()
    };
    let note = format!("attention is {attention}");
    (level, note)
}

/// The resolved value of a branch's `attention` tag — the agent's self-report —
/// or `ok` when the tag is absent (absence is the calm state).
async fn attention_value(state: &AppState, branch: &Branch) -> String {
    tags::get(&state.db, &branch.id, tags::ATTENTION_KEY)
        .await
        .ok()
        .flatten()
        .map(|t| t.value)
        .unwrap_or_else(|| "ok".to_string())
}

/// The levels an agent judgement may name. The two storable triage values plus
/// the calm `ok` — recognising `ok` lets a judgement explicitly return the axis
/// to calm (the caller then clears the tag rather than marking).
const JUDGED_LEVELS: &[&str] = &["ok", "attention", "blocked"];

/// Parse an agent judgement into `(level, note)`. Lenient by design: the first
/// recognised triage word is the level; the rest of that line (or the next
/// non-empty line) is the note. Returns `None` when no level is found, so the
/// caller falls back to the rule.
fn parse_judgement(out: &str) -> Option<(String, String)> {
    for line in out.lines() {
        let lower = line.to_ascii_lowercase();
        for level in JUDGED_LEVELS {
            if lower
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|w| w == *level)
            {
                let note = line
                    .split_once([':', '-'])
                    .map(|x| x.1.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(line.trim())
                    .to_string();
                return Some((level.to_string(), note));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// run_agent — a fresh, env-stripped headless `claude -p` for judgement
// ---------------------------------------------------------------------------

/// Markers of the *calling* Claude Code session, stripped before spawning the
/// sub-agent so it runs fresh and isolated (the lint-review precedent). Mirrors
/// `scripts/lint-review.py`'s `STRIPPED_ENV`.
const STRIPPED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
];

/// Spawn a one-shot headless agent for a judgement call: write `prompt` to its
/// stdin, capture stdout, strip the calling session's env markers. Best-effort:
/// returns `None` when the agent is absent, errors, or times out — the round
/// must work fully without it, so a missing `claude` never breaks a round.
///
/// The command is `WEAVER_OVERLOOKER_AGENT_CMD` (default `claude -p`); a model
/// configured on the overlooker is appended as `--model`.
async fn run_agent(state: &AppState, o: &Overlooker, prompt: &str) -> Option<String> {
    let cmd_str =
        std::env::var("WEAVER_OVERLOOKER_AGENT_CMD").unwrap_or_else(|_| "claude -p".to_string());
    let mut parts = cmd_str.split_whitespace();
    let program = parts.next()?;
    let mut args: Vec<String> = parts.map(str::to_string).collect();
    if !o.model.is_empty() {
        args.push("--model".to_string());
        args.push(o.model.clone());
    }

    let mut command = tokio::process::Command::new(program);
    command
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for key in STRIPPED_ENV {
        command.env_remove(key);
    }

    let mut child = command.spawn().ok()?; // claude not on PATH → None, round falls back.
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(prompt.as_bytes()).await;
        // Drop stdin so the agent sees EOF and proceeds.
        drop(stdin);
    }

    let budget = get_int(&state.db, "overlooker.default_timeout_secs", 600)
        .await
        .max(1) as u64;
    let out = tokio::time::timeout(Duration::from_secs(budget), child.wait_with_output()).await;
    match out {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).to_string())
        }
        _ => None,
    }
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
    fn parse_judgement_finds_level_and_note() {
        let (level, note) = parse_judgement("blocked: stuck retrying the same test").unwrap();
        assert_eq!(level, "blocked");
        assert_eq!(note, "stuck retrying the same test");
        // No recognised level → None (caller falls back to the rule).
        assert!(parse_judgement("looks fine to me, carry on").is_none());
        // 'ok' is recognised even without a separator.
        let (level, _) = parse_judgement("ok").unwrap();
        assert_eq!(level, "ok");
    }
}
