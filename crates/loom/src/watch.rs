//! The Watch **engine** — the timer, dispatcher, and round executor that
//! run inside the loom daemon (the single owner of the terminal/session runtime).
//!
//! The storage + model (`Watch`, `Trigger`, `Scope`, the run audit) lives
//! in [`weaver_core::watch`]; this module is the live machinery that turns
//! those rows into action. It is built as **two halves of one event loop**, the
//! design of record in `docs/plans/watches.md`:
//!
//! * **The timer (producer).** For each enabled scheduled watch it keeps a
//!   `next_run_at`, and when due writes a `cron` system event into the same
//!   `events` stream session changes flow through — nothing more. A cron tick is
//!   a first-class, logged row.
//! * **The dispatcher (consumer).** A sibling of [`crate::monitor::run`] on its
//!   own independent watermark: it reads `events::since`, and for each new event
//!   fires every enabled watch whose trigger matches — a scheduled one
//!   matches its own `cron` tick, a reactive one matches a `tag` write (the
//!   tag's key/value become the trigger's match kind/level, so `attention` and
//!   `triage` tags still drive `{event:"attention"|"triage"}` triggers) or a
//!   `stale` tick, and `manual` ticks (operator "run now") fire the named
//!   watch.
//!
//! Both halves are folded into one [`run`] loop, self-gated on the
//! `watch.enabled` master switch so the daemon can always spawn it and it
//! idles cheaply when off.
//!
//! A **round** is one execution ([`fire`]). It is **level-triggered**: the event
//! that woke it is only a nudge to re-survey the *current* scoped fleet — it
//! never "handles" the specific event, so firing twice is idempotent. The round
//! runs a **program** under the non-optional guardrails — no-overlap, cooldown,
//! timeout, no-recursion — and records every mutating action as both an
//! `watch_runs` action entry and an `events` row (the audit rule). Two
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
use weaver_core::watch::{self as watch_store, Watch};

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

/// The set of watch ids with a round currently in flight. Shared across the
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

/// A round-scoped guard that removes its watch id from the in-flight set on
/// drop, so a panicking or early-returning round can never wedge the set and
/// block every future round of that watch.
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
// Builtin seeding
// ---------------------------------------------------------------------------

/// Ensure every builtin program has its watch row, enabled and on the
/// program's suggested defaults. Runs at engine start, keyed by name: a watch
/// the user has since reconfigured or disabled is left exactly as it is (the
/// row exists), and a deleted one comes back on the next boot — builtins are
/// stock; the off switch is the enabled toggle, not deletion.
pub async fn seed_builtins(db: &crate::Db) -> anyhow::Result<()> {
    for b in crate::builtins::BUILTINS {
        if watch_store::get_by_name(db, b.name).await?.is_some() {
            continue;
        }
        let new = watch_store::NewWatch {
            name: b.name.to_string(),
            trigger_spec: b.default_trigger.to_string(),
            scope: b.default_scope.to_string(),
            program: b.program(),
            params: b.default_params.to_string(),
            capabilities: b
                .default_capabilities
                .iter()
                .map(|c| c.to_string())
                .collect(),
            enabled: true,
            ..Default::default()
        };
        let w = watch_store::create(db, &new).await?;
        tracing::info!(watch = %w.id, name = %w.name, "seeded builtin watch");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The engine loop (timer + dispatcher)
// ---------------------------------------------------------------------------

/// The background engine: spawned in [`crate::server::serve`] alongside the
/// monitor and the GitHub poller. One loop drives both halves — the timer emits
/// `cron` ticks, then the dispatcher drains new events and fires matching
/// watches — so cron and reactive triggers share exactly one code path.
pub async fn run(state: AppState) {
    if let Err(e) = seed_builtins(&state.db).await {
        tracing::warn!("watch: seeding builtin watches failed: {e}");
    }
    let in_flight = new_in_flight();
    // Independent watermark: process every event written after this id. Init
    // from the current max so a restart self-heals on the next tick rather than
    // replaying history (level-triggered — the round always re-surveys).
    let mut last_event = events::max_id(&state.db).await.unwrap_or(0);
    tracing::info!(tick_ms = TICK.as_millis() as u64, "watch engine started");

    loop {
        tokio::time::sleep(TICK).await;

        // Master switch: when off, idle cheaply but keep the watermark current
        // so flipping it on doesn't replay the backlog accumulated while off.
        if !core_config::get_bool(
            &state.db,
            "watch.enabled",
            core_config::DEFAULT_WATCH_ENABLED,
        )
        .await
        {
            last_event = events::max_id(&state.db).await.unwrap_or(last_event);
            continue;
        }

        // 1. Timer (producer): emit `cron` ticks for any scheduled watch
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
            Err(e) => tracing::warn!("watch: reading new events failed: {e}"),
        }
    }
}

/// The timer half: for each enabled scheduled watch, compute (and persist)
/// its `next_run_at` if missing, and when it is due emit a `cron` system tick and
/// advance the schedule. Self-gating on the master switch happens in [`run`].
///
/// Public so a test can drive one timer pass without the loop's master-switch
/// gate or tick cadence; in production only [`run`] calls it.
pub async fn tick_timer(state: &AppState) {
    let watches = match watch_store::list_enabled(&state.db).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("watch timer: listing enabled failed: {e}");
            return;
        }
    };
    let now = Utc::now();
    for o in watches {
        // Dynamic one-shot wake (any watch, scheduled or reactive): a round
        // asked to re-run at `wake_at`. Record the tick first and only clear the
        // column once it lands — a failed insert then leaves the wake set to retry
        // next pass, rather than silently losing it. The clear still happens in
        // this same (awaited) pass, before the round runs, so it stays one-shot.
        if let Some(wake) = o.wake_at.as_deref().and_then(parse_iso) {
            if wake <= now {
                tracing::debug!(watch = %o.id, name = %o.name, "watch timer: wake due; emitting tick");
                match events::record_system(
                    &state.db,
                    &state.bus,
                    "cron",
                    json!({ "watch": o.id, "wake": true }),
                )
                .await
                {
                    Ok(_) => {
                        if let Err(e) = watch_store::set_wake_at(&state.db, &o.id, None).await {
                            // A failed clear leaves the wake set, so next pass
                            // re-fires it — a benign idempotent re-survey (under
                            // no-overlap), but surface it rather than swallow it.
                            tracing::warn!(watch = %o.id, "watch timer: clearing wake_at failed: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(watch = %o.id, "watch timer: recording wake tick failed: {e}");
                    }
                }
                continue;
            }
        }
        let trigger = o.trigger();
        if !trigger.is_scheduled() {
            continue;
        }
        // Seed a never-scheduled watch's next-fire without firing it now.
        let next = match o.next_run_at.as_deref() {
            Some(ts) => parse_iso(ts),
            None => None,
        };
        let next = match next {
            Some(n) => n,
            None => {
                if let Some(n) = next_fire(&o, now) {
                    let _ = watch_store::set_schedule(&state.db, &o.id, None, Some(&iso(n))).await;
                }
                continue;
            }
        };
        if next > now {
            continue;
        }
        // Due: emit the cron tick (the dispatcher fires the round) and advance.
        tracing::debug!(watch = %o.id, name = %o.name, "watch timer: cron due; emitting tick");
        if let Err(e) =
            events::record_system(&state.db, &state.bus, "cron", json!({ "watch": o.id })).await
        {
            tracing::warn!(watch = %o.id, "watch timer: recording cron tick failed: {e}");
            continue;
        }
        let advanced = next_fire(&o, now).map(iso);
        let _ = watch_store::set_schedule(&state.db, &o.id, None, advanced.as_deref()).await;
    }
}

/// The triggering context handed to a round: the normalized event that woke it,
/// its level, and the one session/branch/repo it concerns (a reactive event
/// names one branch; a `cron`/`manual` tick names none). It rides in the round
/// config as `trigger`, so a reactive program can act on just the session that
/// changed — `rnd.triggered_sessions()` — instead of re-surveying the whole
/// fleet, which is what keeps a watch like the PR labeller from hitting the
/// GitHub API once per session every tick.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TriggerCtx {
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
}

impl TriggerCtx {
    /// The context for a scheduled (`cron`/`every`) round — no single session.
    fn scheduled() -> Self {
        Self {
            event: "cron".to_string(),
            ..Default::default()
        }
    }

    /// The context for an operator "run now" — no single session.
    pub fn manual() -> Self {
        Self {
            event: "manual".to_string(),
            ..Default::default()
        }
    }

    /// The context for a reactive round woken by `event` — public so a test can
    /// drive [`fire`] directly without routing a stream event through
    /// [`dispatch`].
    pub fn reactive(event: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            ..Default::default()
        }
    }
}

/// Route one new event to the watches it should fire.
///
/// * a `cron` system tick carries `{watch}` → fire that one (scheduled);
/// * a `manual` system tick carries `{watch, dry_run, reason}` → fire it
///   (operator "run now"), bypassing cooldown;
/// * any other event is **normalized** to a trigger-event name (e.g. a `tag`
///   write of the `attention` key → `session.attention`, a `pr_merged` →
///   `pr.merged`): for each enabled watch whose subscription set names it,
///   fire a (level-triggered) re-survey, handing it the triggering session.
///
/// Public so a test (and the engine loop) can route a single event without the
/// full tick cadence; in production only [`run`] calls it.
pub async fn dispatch(state: &AppState, in_flight: &InFlight, ev: &events::Event) {
    match ev.kind.as_str() {
        "cron" => {
            if let Some(o) = resolve_target(state, ev).await {
                // A wake tick (a watch's own self-scheduled recheck) carries
                // `wake:true` and fires under reason "wake", which bypasses
                // cooldown — the delay the round chose IS its self-imposed gap, so
                // a global cooldown must not swallow the recheck and strand the
                // backoff. A plain scheduled tick stays "cron" and is gated.
                let reason = if ev
                    .data
                    .get("wake")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "wake"
                } else {
                    "cron"
                };
                tracing::debug!(watch = %o.id, name = %o.name, reason, "dispatching cron tick to watch");
                let _ = fire(
                    state,
                    in_flight,
                    &o,
                    reason,
                    false,
                    &TriggerCtx::scheduled(),
                )
                .await;
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
                tracing::info!(watch = %o.id, name = %o.name, reason, dry_run, "dispatching manual run to watch");
                let _ = fire(state, in_flight, &o, reason, dry_run, &TriggerCtx::manual()).await;
            }
        }
        // The engine's own audit rows (and a `cron`/`manual` already handled)
        // must never re-trigger a reactive round, or it would chase its tail.
        "watch" => {}
        ev_kind => {
            // Reactive: normalize the raw stream event into the watch-facing
            // trigger vocabulary. An event that maps to no trigger event (a
            // free-form tag, a `created`/`launching` status) wakes nothing.
            let Some((event, level)) = trigger_event_of(ev, ev_kind) else {
                return;
            };
            let repo = event_repo(state, ev).await;
            let session = triggering_session(state, ev).await;
            tracing::debug!(
                event = %event,
                level = ?level,
                branch = %ev.branch_id,
                session = ?session,
                repo = ?repo,
                "watch dispatch: normalized reactive event"
            );
            let watches = match watch_store::list_enabled(&state.db).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!("watch dispatch: listing enabled failed: {e}");
                    return;
                }
            };
            for o in watches {
                if o.trigger()
                    .matches_event(&event, level.as_deref(), repo.as_deref())
                {
                    let ctx = TriggerCtx {
                        event: event.clone(),
                        level: level.clone(),
                        session: session.clone(),
                        branch: (!events::is_system(&ev.branch_id)).then(|| ev.branch_id.clone()),
                        repo: repo.clone(),
                    };
                    tracing::debug!(
                        watch = %o.id,
                        name = %o.name,
                        event = %event,
                        branch = ?ctx.branch,
                        session = ?ctx.session,
                        "watch trigger matched; dispatching round"
                    );
                    let _ =
                        fire(state, in_flight, &o, &format!("event:{event}"), false, &ctx).await;
                }
            }
        }
    }
}

/// Normalize a raw stream event into its watch-facing **trigger event** name and
/// level, or `None` when the raw event isn't a subscription surface. This is the
/// one place the internal event stream (`status`/`hook`/`tag`/`stale`/`pr_*`) is
/// mapped to the stable, documented vocabulary scripts subscribe to — adding a
/// new trigger event is a line here, not a change every script must learn.
fn trigger_event_of(ev: &events::Event, ev_kind: &str) -> Option<(String, Option<String>)> {
    let str_field = |key: &str| ev.data.get(key).and_then(Value::as_str);
    match ev_kind {
        // A tag write carries `{key, value}`: the agent's `attention` self-report
        // and a watch's `triage` mark are the two trigger surfaces; a cleared tag
        // (empty value) presents level `ok`.
        "tag" => {
            let level = str_field("value")
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or_else(|| Some("ok".to_string()));
            match str_field("key")? {
                "attention" => Some(("session.attention".to_string(), level)),
                "triage" => Some(("triage.changed".to_string(), level)),
                _ => None,
            }
        }
        // Status transitions are edge-emitted (only on real change): `running`
        // is the agent starting work; the terminal states are an exit (the level
        // names which).
        "status" => match str_field("status")? {
            "running" => Some(("session.started".to_string(), None)),
            status @ ("done" | "error" | "archived" | "orphaned") => {
                Some(("session.exited".to_string(), Some(status.to_string())))
            }
            _ => None,
        },
        // The `idle` Claude hook is a finished turn — the closest signal to
        // "round complete". `working`/`waiting`/`session-start` drive status and
        // attention instead, so they aren't duplicated here.
        "hook" => match str_field("event")? {
            "idle" => Some(("session.idle".to_string(), None)),
            _ => None,
        },
        "stale" => Some(("session.stale".to_string(), None)),
        "pr_red" => Some(("pr.checks_red".to_string(), None)),
        "pr_green" => Some(("pr.checks_green".to_string(), None)),
        "pr_opened" => Some(("pr.opened".to_string(), None)),
        "pr_merged" => Some(("pr.merged".to_string(), None)),
        "pr_review" => Some((
            "pr.review_changed".to_string(),
            str_field("decision").map(str::to_string),
        )),
        _ => None,
    }
}

/// The named watch carried by a `cron`/`manual` system tick's `{watch}`
/// field, if it still exists and is enabled.
async fn resolve_target(state: &AppState, ev: &events::Event) -> Option<Watch> {
    let key = ev.data.get("watch").and_then(Value::as_str)?;
    let o = watch_store::resolve(&state.db, key).await.ok().flatten()?;
    o.enabled.then_some(o)
}

/// The `repo_root` an event originates from — the originating branch's repo, for
/// the trigger's optional repo filter. System events have no branch, so no repo.
async fn event_repo(state: &AppState, ev: &events::Event) -> Option<String> {
    if events::is_system(&ev.branch_id) {
        return None;
    }
    branch_mod::get(&state.db, &ev.branch_id)
        .await
        .ok()
        .flatten()
        .map(|b| b.repo_root)
}

/// The session a reactive event concerns: the explicit `data.session` when the
/// event carries one (e.g. `stale`), else the active session of the event's
/// branch. `None` for a system event. Handed to the round so it can act on the
/// one session that changed.
async fn triggering_session(state: &AppState, ev: &events::Event) -> Option<String> {
    if let Some(sid) = ev.data.get("session").and_then(Value::as_str) {
        return Some(sid.to_string());
    }
    if events::is_system(&ev.branch_id) {
        return None;
    }
    session_mod::active_for_branch(&state.db, &ev.branch_id)
        .await
        .ok()
        .flatten()
        .map(|s| s.id)
}

// ---------------------------------------------------------------------------
// The round executor + guardrails
// ---------------------------------------------------------------------------

/// Execute one round of `o`. The single code path for every trigger — cron,
/// manual, reactive. Returns the run id, or `None` when a guardrail skipped the
/// round before a run row was opened (no-overlap / cooldown).
///
/// Guardrails, in order:
/// 1. **no-overlap** — a re-fire while a round of the same watch is in
///    flight is dropped (no run row); the in-flight set is the gate.
/// 2. **cooldown** — a fire inside `max(cooldown_secs, default_cooldown_secs)`
///    of the last run is recorded `skipped`. A `manual`/`run-now`, and a watch's
///    own self-scheduled `wake`, bypass it (the wake's delay is the gap).
/// 3. **timeout** — the program is wrapped in a wall-clock budget; an overrun
///    is recorded `error`.
/// 4. **no-recursion** — the survey ([`run_program`]) excludes watch warm
///    sessions, so a watcher never acts on another watcher.
pub async fn fire(
    state: &AppState,
    in_flight: &InFlight,
    o: &Watch,
    trigger_reason: &str,
    dry_run: bool,
    ctx: &TriggerCtx,
) -> Option<i64> {
    // 1. No-overlap: claim the in-flight slot or drop silently. A dropped round
    //    is intentionally not a run row — it never started.
    {
        let mut set = in_flight.lock().await;
        if !set.insert(o.id.clone()) {
            tracing::debug!(watch = %o.id, "watch: round already in flight; skipping re-fire");
            return None;
        }
    }
    let _guard = InFlightGuard {
        set: in_flight.clone(),
        id: o.id.clone(),
    };

    let manual = trigger_reason == "manual" || trigger_reason.starts_with("run");
    // A self-scheduled wake bypasses cooldown too: its backoff delay is the
    // watch's own chosen gap, so a global cooldown must not strand the recheck.
    let bypass_cooldown = manual || trigger_reason == "wake";
    let now = Utc::now();

    // 2. Cooldown: a re-fire inside the gap is recorded `skipped` (a manual run
    //    or a self-scheduled wake bypasses it).
    let cooldown = o
        .cooldown_secs
        .max(get_int(&state.db, "watch.default_cooldown_secs", 0).await);
    if !bypass_cooldown && cooldown > 0 {
        if let Some(last) = o.last_run_at.as_deref().and_then(parse_iso) {
            if (now - last).num_seconds() < cooldown {
                return record_skipped(
                    state,
                    o,
                    trigger_reason,
                    &ctx.event,
                    &format!("cooldown: {cooldown}s gap not elapsed"),
                )
                .await;
            }
        }
    }

    // Open the run row; everything from here closes it via `finish_run`.
    let run_id = match watch_store::start_run(&state.db, &o.id, trigger_reason, &ctx.event).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(watch = %o.id, "watch: opening run row failed: {e}");
            return None;
        }
    };
    tracing::info!(watch = %o.id, run = run_id, trigger = trigger_reason, "watch run started");

    // 3. Timeout: budget the program run. An overrun is `error`, the schedule
    //    still advances so the next trigger fires.
    let timeout_secs = get_int(&state.db, "watch.default_timeout_secs", 600)
        .await
        .max(1);
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs as u64),
        run_program(state, o, dry_run, ctx),
    )
    .await;

    let round = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => RoundResult::failed(format!("round failed: {e}")),
        Err(_) => RoundResult::failed(format!("round exceeded {timeout_secs}s budget")),
    };

    let _ = watch_store::finish_run(&state.db, run_id, &round.record()).await;
    tracing::info!(watch = %o.id, run = run_id, outcome = %round.outcome, "watch run completed");
    // Stamp the schedule: last_run_at = now; advance next_run_at for a scheduled
    // watch (a reactive one keeps None).
    let next = next_fire(o, now).map(iso);
    let _ = watch_store::set_schedule(&state.db, &o.id, Some(&iso(now)), next.as_deref()).await;

    // Persist the program's lookaside-state write and its dynamic-wake request.
    // These are how a round carries memory forward and self-schedules its next
    // look (e.g. an exponential-backoff recheck) — independent of the cron
    // cadence above.
    if let Some(new_state) = &round.state {
        let _ = watch_store::set_state(&state.db, &o.id, new_state).await;
    }
    match round.wake {
        Wake::Leave => {}
        Wake::Clear => {
            let _ = watch_store::set_wake_at(&state.db, &o.id, None).await;
        }
        Wake::In(secs) => {
            let wake_at = iso(now + chrono::Duration::seconds(secs.max(1)));
            let _ = watch_store::set_wake_at(&state.db, &o.id, Some(&wake_at)).await;
        }
    }

    Some(run_id)
}

/// Record a `skipped` round (cooldown / a guardrail that still merits an audit
/// row, unlike the silent no-overlap drop). Returns the run id.
async fn record_skipped(
    state: &AppState,
    o: &Watch,
    trigger_reason: &str,
    trigger_event: &str,
    summary: &str,
) -> Option<i64> {
    let run_id = watch_store::start_run(&state.db, &o.id, trigger_reason, trigger_event)
        .await
        .ok()?;
    tracing::info!(watch = %o.id, name = %o.name, run = run_id, summary, "watch round skipped");
    let _ = watch_store::finish_run(
        &state.db,
        run_id,
        &watch_store::RunRecord {
            outcome: "skipped",
            summary,
            actions: &Value::Array(vec![]),
            stdout: "",
            stderr: "",
            exit_code: None,
            duration_ms: None,
        },
    )
    .await;
    Some(run_id)
}

// ---------------------------------------------------------------------------
// The program substrate — the subprocess script executor
// ---------------------------------------------------------------------------

/// What a round's result asks the engine to do with the watch's one-shot
/// dynamic wake (`wake_at`). The program controls it via the `wake_in` field of
/// its result JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wake {
    /// No `wake_in` in the result — leave `wake_at` as it was. Used by every
    /// program that never self-schedules, and by the error/timeout paths so a
    /// transient script failure never drops a pending backoff recheck.
    Leave,
    /// `wake_in: 0` (or negative) — the program is done; clear any pending wake.
    Clear,
    /// `wake_in: N` (> 0) — re-fire this watch in `N` seconds.
    In(i64),
}

/// The result of running a program: the outcome + one-line summary + actions
/// for the audit trail, plus the captured execution log (stdout/stderr tails,
/// the interpreter exit code, and the wall-clock it ran), and the program's
/// lookaside-state write + dynamic-wake request (both engine bookkeeping, not
/// part of the audit record).
struct RoundResult {
    outcome: String,
    summary: String,
    actions: Value,
    stdout: String,
    stderr: String,
    exit_code: Option<i64>,
    duration_ms: Option<i64>,
    /// The new lookaside state to persist (`None` = the program returned none,
    /// leave the stored state untouched).
    state: Option<Value>,
    /// What to do with the dynamic wake timer.
    wake: Wake,
}

impl RoundResult {
    /// A failed round with no captured output — a guardrail (timeout) or a spawn
    /// error that produced no streams. The script's own non-zero exit is a
    /// different path: [`run_script`] returns an `error` result that *does* carry
    /// the streams, so the execution log shows what it printed.
    fn failed(summary: String) -> Self {
        Self {
            outcome: "error".to_string(),
            summary,
            actions: Value::Array(vec![]),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            duration_ms: None,
            state: None,
            wake: Wake::Leave,
        }
    }

    /// Borrow this result as the [`watch_store::RunRecord`] `finish_run` stores.
    fn record(&self) -> watch_store::RunRecord<'_> {
        watch_store::RunRecord {
            outcome: &self.outcome,
            summary: &self.summary,
            actions: &self.actions,
            stdout: &self.stdout,
            stderr: &self.stderr,
            exit_code: self.exit_code,
            duration_ms: self.duration_ms,
        }
    }
}

/// Resolve the program a watch names into its script source: a builtin
/// embedded from [`crate::builtins`], or a custom program file (an absolute
/// path, conventionally under `~/.weaver/watches/`). A bare relative path
/// or unknown builtin errors.
fn resolve_source(program: &str) -> anyhow::Result<ScriptSource> {
    if let Some(source) = crate::builtins::find(program).map(|b| b.source) {
        return Ok(ScriptSource::Embedded {
            file_name: program_file_name(program),
            source,
        });
    }
    let path = std::path::PathBuf::from(program);
    if path.is_absolute() {
        return Ok(ScriptSource::File(path));
    }
    Err(anyhow::anyhow!(
        "unknown watch program '{program}' — expected 'builtin:<name>' or an absolute path"
    ))
}

/// Run the program the watch names for one round, handing it the triggering
/// context so a reactive program can scope to the session that changed.
async fn run_program(
    state: &AppState,
    o: &Watch,
    dry_run: bool,
    ctx: &TriggerCtx,
) -> anyhow::Result<RoundResult> {
    run_script(state, o, dry_run, resolve_source(&o.program)?, ctx).await
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
/// and `$WEAVER_WATCH` the round config (`{id, name, program, params,
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
    o: &Watch,
    dry_run: bool,
    src: ScriptSource,
    ctx: &TriggerCtx,
) -> anyhow::Result<RoundResult> {
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
        "mode": "run",
        "trigger": serde_json::to_value(ctx).unwrap_or(Value::Null),
        "state": o.state(),
    });

    // A spawn / scratch-setup failure (no interpreter, unreadable file) is a
    // round error with no streams — propagated up; the script's own non-zero
    // exit is recorded below *with* its captured output, so the log shows it.
    let out = spawn_script(state, &src, &config).await?;
    let stdout = cap_stream(&out.stdout);
    let stderr = cap_stream(&out.stderr);
    let duration_ms = Some(out.duration_ms);

    if !out.success {
        // A script crash leaves state and the dynamic wake untouched: a transient
        // failure must not drop a pending backoff recheck.
        return Ok(RoundResult {
            outcome: "error".to_string(),
            summary: format!(
                "script exited with {}: {}",
                out.exit_code.unwrap_or(-1),
                tail(&out.stderr, 400)
            ),
            actions: Value::Array(vec![]),
            stdout,
            stderr,
            exit_code: out.exit_code,
            duration_ms,
            state: None,
            wake: Wake::Leave,
        });
    }

    let result = match parse_round_result(&out.stdout) {
        Some(parsed) => RoundResult {
            outcome: parsed.outcome,
            summary: parsed.summary,
            actions: parsed.actions,
            stdout,
            stderr,
            exit_code: out.exit_code,
            duration_ms,
            state: parsed.state,
            wake: parsed.wake,
        },
        None => RoundResult {
            outcome: "error".to_string(),
            summary: format!(
                "script printed no result JSON object ({{outcome, summary, actions}}); stdout: {}; stderr: {}",
                tail(&out.stdout, 200),
                tail(&out.stderr, 200)
            ),
            actions: Value::Array(vec![]),
            stdout,
            stderr,
            exit_code: out.exit_code,
            duration_ms,
            state: None,
            wake: Wake::Leave,
        },
    };
    Ok(result)
}

/// The captured output of one script spawn: its exit status, stdout/stderr, and
/// the wall-clock it ran. Shared by the round executor and register-mode
/// manifest evaluation.
struct ScriptOutput {
    success: bool,
    exit_code: Option<i64>,
    stdout: String,
    stderr: String,
    duration_ms: i64,
}

/// Spawn a script subprocess and capture its output. An env-stripped process
/// (the lint-review precedent, like [`crate::agent::run_oneshot`]) that reaches
/// the fleet only through the loom REST API: `$WEAVER_API` carries the daemon's
/// own address and `$WEAVER_WATCH` the JSON `config`; the vendored
/// `weaver_loom` module rides `PYTHONPATH` so a program imports the API layer
/// with no install step. The interpreter is `python3`, or `uv run --script` when
/// the script declares PEP 723 inline metadata and `uv` is installed.
///
/// `config.mode` selects what the script does: `run` executes a round (and is
/// expected to print `{outcome, summary, actions}`); `register` asks it to print
/// its subscription manifest instead. `kill_on_drop` reaps the subprocess if the
/// caller's budget cancels the future.
async fn spawn_script(
    state: &AppState,
    src: &ScriptSource,
    config: &Value,
) -> anyhow::Result<ScriptOutput> {
    // One scratch dir per spawn: the vendored module always lands here (for
    // PYTHONPATH), an embedded builtin's source too (so a traceback carries a
    // real file/line). Removed when the spawn ends.
    let scratch = tempfile::tempdir().map_err(|e| anyhow::anyhow!("creating scratch dir: {e}"))?;
    let module = scratch.path().join("weaver_loom.py");
    tokio::fs::write(&module, crate::builtins::PYTHON_MODULE)
        .await
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", module.display()))?;

    let (script_path, source) = match src {
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

    let mode = config
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("run")
        .to_string();
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
        .env("WEAVER_WATCH", config.to_string())
        .env("WEAVER_WATCH_MODE", &mode)
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

    let started = std::time::Instant::now();
    let out = command.output().await.map_err(|e| {
        anyhow::anyhow!("spawning {interpreter} failed: {e} (is {interpreter} installed?)")
    })?;
    Ok(ScriptOutput {
        success: out.status.success(),
        exit_code: out.status.code().map(i64::from),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        duration_ms: started.elapsed().as_millis() as i64,
    })
}

/// Evaluate a program's **subscription manifest** by running it in register
/// mode: the script prints its `{cron?, every?, on:[...]}` declaration instead
/// of running a round, and that becomes the watch's stored trigger. Returns
/// `None` when the program isn't a manifest-declaring script (a legacy script
/// that prints a round result), so the caller keeps the existing/default
/// trigger. Errors only on a spawn failure or a non-zero register exit.
///
/// This is the reconcile step: the engine calls it when a watch is created or
/// its program changes, so the script — not whoever wired it up — owns which
/// events wake it.
pub async fn evaluate_manifest(
    state: &AppState,
    program: &str,
    params: &Value,
) -> anyhow::Result<Option<watch_store::Trigger>> {
    let src = resolve_source(program)?;
    let config = json!({ "mode": "register", "program": program, "params": params });
    let out = spawn_script(state, &src, &config).await?;
    if !out.success {
        anyhow::bail!(
            "register mode exited with {}: {}",
            out.exit_code.unwrap_or(-1),
            tail(&out.stderr, 400)
        );
    }
    Ok(parse_manifest(&out.stdout))
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

/// The largest stored stdout/stderr tail per run — enough to read a traceback
/// or a chatty progress log without an unbounded script bloating a row.
const STREAM_CAP: usize = 64_000;

/// Cap a captured stream to its [`STREAM_CAP`] tail for storage.
fn cap_stream(s: &str) -> String {
    tail(s, STREAM_CAP)
}

/// The last JSON object on stdout (the whole thing, or — so a script may log
/// progress lines first — the last line that is one).
fn parse_last_json_object(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    let as_object = |t: &str| {
        serde_json::from_str::<Value>(t.trim())
            .ok()
            .filter(Value::is_object)
    };
    as_object(trimmed).or_else(|| trimmed.lines().rev().find_map(as_object))
}

/// The fields a script's result JSON contributes to a [`RoundResult`]. Named
/// (not a bare tuple) so the two same-typed `String` slots — `outcome` and
/// `summary` — can't be transposed at a call site.
struct ParsedRound {
    outcome: String,
    summary: String,
    actions: Value,
    /// The lookaside state to persist, if the script returned a `state` object
    /// (`None` = absent, leave the stored state alone).
    state: Option<Value>,
    /// The dynamic-wake request from the script's optional `wake_in`.
    wake: Wake,
}

/// Parse a script's stdout into its [`ParsedRound`]. The fields are read
/// leniently: a missing/unknown `outcome` reads as `ok`, a missing `summary` as
/// empty, a missing/non-array `actions` as none — so a minimal script stays
/// minimal. The optional `state` (an object) and `wake_in` (seconds) drive the
/// engine's lookaside-state and dynamic-wake bookkeeping.
fn parse_round_result(stdout: &str) -> Option<ParsedRound> {
    let obj = parse_last_json_object(stdout)?;
    let obj = obj.as_object()?;
    let outcome = obj
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|o| watch_store::OUTCOMES.contains(o))
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
    // Only an object is accepted as state; anything else is ignored (leave).
    let state = obj.get("state").filter(|s| s.is_object()).cloned();
    // `wake_in` absent → leave; <= 0 → clear; > 0 → re-fire in that many seconds.
    let wake = match obj.get("wake_in").and_then(Value::as_i64) {
        None => Wake::Leave,
        Some(secs) if secs > 0 => Wake::In(secs),
        Some(_) => Wake::Clear,
    };
    Some(ParsedRound {
        outcome: outcome.to_string(),
        summary: summary.to_string(),
        actions,
        state,
        wake,
    })
}

/// Parse a script's register-mode stdout into a [`watch_store::Trigger`] subscription
/// manifest, or `None` when it isn't one. A round result (an object carrying
/// `outcome`/`actions`) is *not* a manifest — that is a legacy script that
/// doesn't declare subscriptions, so the caller keeps the existing trigger. An
/// explicit (even empty) `{cron?, every?, on?}` object is accepted.
fn parse_manifest(stdout: &str) -> Option<watch_store::Trigger> {
    let value = parse_last_json_object(stdout)?;
    let obj = value.as_object()?;
    if obj.contains_key("outcome") || obj.contains_key("actions") {
        return None;
    }
    serde_json::from_value::<watch_store::Trigger>(value).ok()
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

/// Fire `key` (a watch id or name) now, returning the run id. Used by the
/// operator's `loom watch run` (a later task) and by tests to drive a round
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
    let o = watch_store::resolve(&state.db, key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no watch '{key}'"))?;
    // A fresh, request-scoped in-flight set: a direct operator run is its own
    // no-overlap domain (the engine loop guards its own concurrent fires).
    let in_flight = new_in_flight();
    let reason = if reason.is_empty() { "manual" } else { reason };
    tracing::info!(watch = %o.id, name = %o.name, reason, dry_run, "operator fired watch now");
    fire(
        state,
        &in_flight,
        &o,
        reason,
        dry_run,
        &TriggerCtx::manual(),
    )
    .await
    .ok_or_else(|| anyhow::anyhow!("round skipped before it could open a run row"))
}

// ---------------------------------------------------------------------------
// Warm-session lifecycle (T12)
// ---------------------------------------------------------------------------

/// Ensure the warm watch `o` has its long-lived, engine-managed session,
/// returning its id. **Idempotent and reuse-first**: if `o` already owns a live
/// managed session, that id is returned (and re-linked into `warm_session_id` if
/// it had drifted) — no duplicate is spawned. The session id is stable across
/// rounds and across a daemon restart, which is what gives the watch its
/// across-round memory.
///
/// On first need it forks a dedicated worktree and brings up a real terminal session
/// (via [`crate::web::create_warm_session`], the same launch machinery ordinary
/// sessions use), stamps it `managed_by = o.id` so the fleet hides it, and
/// records its id on the watch.
///
/// A non-warm watch (`params.warm` unset) returns `Ok(None)` without
/// spawning anything. The repo to anchor the worktree is the watch's
/// `scope.repo`, else the most recently used repo; a watch with no repo to
/// anchor errors rather than guessing.
pub async fn ensure_warm_session(state: &AppState, o: &Watch) -> anyhow::Result<Option<String>> {
    if !o.warm() {
        return Ok(None);
    }

    // Reuse-first: an existing live managed session is the warm session. Keep its
    // id and (cheaply) repair the watch linkage if it drifted.
    if let Some(existing) = session_mod::active_managed_by(&state.db, &o.id).await? {
        if o.warm_session_id.as_deref() != Some(existing.id.as_str()) {
            tracing::info!(watch = %o.id, session = %existing.id, "repairing drifted warm session linkage");
            watch_store::set_warm_session(&state.db, &o.id, Some(&existing.id)).await?;
        } else {
            tracing::debug!(watch = %o.id, session = %existing.id, "reusing existing warm session");
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
    watch_store::set_warm_session(&state.db, &o.id, Some(&session.id)).await?;
    tracing::info!(watch = %o.id, session = %session.id, "watch warm session created");
    Ok(Some(session.id))
}

// ---------------------------------------------------------------------------
// Schedule arithmetic (cron + `every` sugar)
// ---------------------------------------------------------------------------

/// The next fire time for a scheduled watch after `from`. A `cron` field is
/// parsed with `croner` (standard 5-field crontab); an `every` field is the
/// duration sugar (`30m`, `2h`, `45s`). A reactive (non-scheduled) watch
/// has no next fire.
fn next_fire(o: &Watch, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
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
    fn parse_round_result_reads_state_and_wake() {
        // No state / no wake_in → leave both alone.
        let r = parse_round_result(r#"{"outcome":"ok"}"#).unwrap();
        assert!(r.state.is_none());
        assert_eq!(r.wake, Wake::Leave);

        // A state object is captured; wake_in > 0 schedules a re-fire.
        let r = parse_round_result(r#"{"outcome":"ok","state":{"s":{"attempts":2}},"wake_in":30}"#)
            .unwrap();
        assert_eq!(r.state.unwrap(), serde_json::json!({"s":{"attempts":2}}));
        assert_eq!(r.wake, Wake::In(30));

        // wake_in 0 (or negative) clears the pending wake — "I'm done".
        let r = parse_round_result(r#"{"outcome":"noop","wake_in":0}"#).unwrap();
        assert_eq!(r.wake, Wake::Clear);
        let r = parse_round_result(r#"{"outcome":"noop","wake_in":-5}"#).unwrap();
        assert_eq!(r.wake, Wake::Clear);

        // A non-object state is ignored rather than persisted as junk.
        let r = parse_round_result(r#"{"outcome":"ok","state":"nope"}"#).unwrap();
        assert!(r.state.is_none());
    }

    #[test]
    fn parse_manifest_accepts_declarations_not_round_results() {
        // A subscription manifest parses into a Trigger.
        let t = parse_manifest(r#"{"on":["pr.merged","pr.opened"]}"#).unwrap();
        assert_eq!(t.on, vec!["pr.merged", "pr.opened"]);
        let t = parse_manifest(r#"{"cron":"0 * * * *"}"#).unwrap();
        assert_eq!(t.cron.as_deref(), Some("0 * * * *"));
        // An empty manifest is a valid "manual only" declaration.
        assert!(parse_manifest("{}").is_some());
        // A round result is NOT a manifest — a legacy script that doesn't
        // declare subscriptions; the caller keeps the existing trigger.
        assert!(parse_manifest(r#"{"outcome":"ok","summary":"x","actions":[]}"#).is_none());
        // A script may log first; the last object wins.
        let t = parse_manifest("registering...\n{\"every\":\"15m\"}\n").unwrap();
        assert_eq!(t.every.as_deref(), Some("15m"));
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

    /// Boot seeding gives every builtin program a watch row, enabled and on
    /// its suggested defaults — and leaves existing rows exactly as they are
    /// on a re-run, so a user's disable/reconfigure survives restarts.
    #[tokio::test]
    async fn seed_builtins_is_idempotent_and_enables_by_default() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_builtins(&db).await.unwrap();
        let all = watch_store::list(&db).await.unwrap();
        assert_eq!(all.len(), crate::builtins::BUILTINS.len());
        for w in &all {
            assert!(w.enabled, "{}: builtins seed enabled", w.name);
            assert_eq!(w.program, format!("builtin:{}", w.name));
        }

        // Disable one, then re-seed: nothing is re-created or re-enabled.
        watch_store::set_enabled(&db, &all[0].id, false)
            .await
            .unwrap();
        seed_builtins(&db).await.unwrap();
        let after = watch_store::list(&db).await.unwrap();
        assert_eq!(after.len(), all.len(), "re-seed adds nothing");
        assert!(!after.iter().find(|w| w.id == all[0].id).unwrap().enabled);

        // A deleted builtin comes back (stock watches re-seed on boot).
        watch_store::delete(&db, &all[0].id).await.unwrap();
        seed_builtins(&db).await.unwrap();
        let restored = watch_store::list(&db).await.unwrap();
        assert_eq!(restored.len(), all.len());
        assert!(restored.iter().any(|w| w.name == all[0].name && w.enabled));
    }

    /// An `AppState` over a fresh in-memory db plus a watch registered on
    /// `program` — the minimum for a [`fire`] round to run a script end to end.
    async fn script_fixture(program: &str) -> (AppState, Watch) {
        let db = crate::db::connect_in_memory().await.unwrap();
        let state = AppState {
            trigger: crate::github_trigger::GithubTrigger::production(db.clone()),
            db,
            bus: events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
        };
        let o = watch_store::create(
            &state.db,
            &watch_store::NewWatch {
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
    /// PYTHONPATH, the `Round` context exposes the `$WEAVER_WATCH` config
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
        let run_id = fire(
            &state,
            &new_in_flight(),
            &o,
            "manual",
            true,
            &TriggerCtx::manual(),
        )
        .await
        .unwrap();
        let run = watch_store::recent_runs(&state.db, &o.id, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == run_id)
            .unwrap();
        assert_eq!(run.outcome, "ok", "summary: {}", run.summary);
        // The execution log captured the script's output and a clean exit.
        assert_eq!(run.exit_code, Some(0));
        assert!(run.duration_ms.is_some());
        assert_eq!(run.trigger_event, "manual");
        assert!(
            run.summary
                .contains("name=script-test label=weaver dry=true api=http://127.0.0.1:0"),
            "the script saw its config through the module: {}",
            run.summary
        );
        let actions: Value = serde_json::from_str(&run.actions).unwrap();
        assert_eq!(actions[0]["would"], "label");
    }

    /// A round that returns `state` and `wake_in` has both persisted on the
    /// watch row: the lookaside state carries to the next round, and the
    /// dynamic wake is armed for a self-scheduled recheck.
    #[tokio::test]
    async fn fire_persists_state_and_wake() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("stateful.py");
        std::fs::write(
            &script,
            r#"
from weaver_loom import Round
rnd = Round()
# Echo the prior state's counter, incremented, and schedule a recheck.
n = (rnd.state.get("n") or 0) + 1
rnd.set_state({"n": n})
rnd.wake_in(30)
rnd.finish("counted to %d" % n)
"#,
        )
        .unwrap();

        let (state, o) = script_fixture(&script.display().to_string()).await;
        // First round: no prior state → n = 1, wake armed.
        fire(
            &state,
            &new_in_flight(),
            &o,
            "manual",
            false,
            &TriggerCtx::manual(),
        )
        .await
        .unwrap();
        let after = watch_store::get(&state.db, &o.id).await.unwrap().unwrap();
        assert_eq!(after.state(), serde_json::json!({"n": 1}));
        assert!(after.wake_at.is_some(), "a wake_in armed wake_at");

        // Second round reads the persisted state back: n = 2.
        fire(
            &state,
            &new_in_flight(),
            &after,
            "manual",
            false,
            &TriggerCtx::manual(),
        )
        .await
        .unwrap();
        let after = watch_store::get(&state.db, &o.id).await.unwrap().unwrap();
        assert_eq!(after.state(), serde_json::json!({"n": 2}));
    }

    /// A self-scheduled `wake` fire bypasses cooldown, where a plain `cron` fire
    /// inside the window is skipped — so a global cooldown can never strand a
    /// backoff recheck the watch armed for itself.
    #[tokio::test]
    async fn wake_bypasses_cooldown_but_cron_is_gated() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("noop.py");
        std::fs::write(
            &script,
            "from weaver_loom import Round\nRound().finish('ok')\n",
        )
        .unwrap();
        let (state, _) = script_fixture(&script.display().to_string()).await;
        // A long cooldown so a back-to-back re-fire would normally be skipped.
        let o = watch_store::create(
            &state.db,
            &watch_store::NewWatch {
                name: "gated".to_string(),
                program: script.display().to_string(),
                cooldown_secs: 3600,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Seed last_run_at via a manual run (manual bypasses cooldown). A fresh
        // in-flight set per fire — each is its own no-overlap domain, sidestepping
        // the async drop of the guard between sequential calls.
        fire(
            &state,
            &new_in_flight(),
            &o,
            "manual",
            false,
            &TriggerCtx::manual(),
        )
        .await
        .unwrap();
        let o = watch_store::get(&state.db, &o.id).await.unwrap().unwrap();

        // A plain cron re-fire inside the window is skipped…
        let cron_id = fire(
            &state,
            &new_in_flight(),
            &o,
            "cron",
            false,
            &TriggerCtx::scheduled(),
        )
        .await
        .unwrap();
        // …but a self-scheduled wake runs anyway.
        let wake_id = fire(
            &state,
            &new_in_flight(),
            &o,
            "wake",
            false,
            &TriggerCtx::scheduled(),
        )
        .await
        .unwrap();

        let runs = watch_store::recent_runs(&state.db, &o.id, 10)
            .await
            .unwrap();
        let outcome = |id| runs.iter().find(|r| r.id == id).unwrap().outcome.as_str();
        assert_eq!(outcome(cron_id), "skipped", "cron is gated by cooldown");
        assert_ne!(outcome(wake_id), "skipped", "wake bypasses cooldown");
    }

    /// A due `wake_at` makes the timer emit one `cron` tick for that watch
    /// and clear the column — the dynamic self-trigger, working for a reactive
    /// watch the scheduled timer would otherwise never visit.
    #[tokio::test]
    async fn timer_fires_a_due_wake_once_and_clears_it() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let state = AppState {
            trigger: crate::github_trigger::GithubTrigger::production(db.clone()),
            db,
            bus: events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
        };
        // A reactive watch (no cron) — the scheduled half of the timer skips
        // it; only the wake half should fire it.
        let o = watch_store::create(
            &state.db,
            &watch_store::NewWatch {
                name: "waker".to_string(),
                trigger_spec: r#"{"on":["session.idle"]}"#.to_string(),
                enabled: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        watch_store::set_wake_at(&state.db, &o.id, Some("2000-01-01T00:00:00.000Z"))
            .await
            .unwrap();

        tick_timer(&state).await;

        // Exactly one cron tick naming this watch, and the wake is cleared.
        let crons: Vec<_> = events::since(&state.db, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.kind == "cron"
                    && e.data.get("watch").and_then(Value::as_str) == Some(o.id.as_str())
            })
            .collect();
        assert_eq!(crons.len(), 1, "one wake tick emitted");
        assert_eq!(
            crons[0].data.get("wake").and_then(Value::as_bool),
            Some(true),
            "the wake tick is marked so it bypasses cooldown"
        );
        assert!(
            watch_store::get(&state.db, &o.id)
                .await
                .unwrap()
                .unwrap()
                .wake_at
                .is_none(),
            "wake_at cleared so it is strictly one-shot"
        );

        // A second pass with no wake set emits no further tick.
        tick_timer(&state).await;
        let crons = events::since(&state.db, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "cron")
            .count();
        assert_eq!(crons, 1, "no re-fire once the wake is consumed");
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
        let run_id = fire(
            &state,
            &new_in_flight(),
            &o,
            "manual",
            false,
            &TriggerCtx::manual(),
        )
        .await
        .unwrap();
        let run = watch_store::recent_runs(&state.db, &o.id, 10)
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
        // A failed round still records its execution log: the traceback on
        // stderr and the non-zero exit code.
        assert!(
            run.stderr.contains("kaboom"),
            "stderr captured: {}",
            run.stderr
        );
        assert_eq!(run.exit_code, Some(1));
    }
}
