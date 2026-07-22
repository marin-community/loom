//! Background task: detects when a session's terminal has ended and consumes the
//! event rows the `weaver` CLI writes — `hook` events (Claude lifecycle) and
//! `tag` events (`weaver status` writing the `attention` tag) — reflecting
//! them onto the session and the dashboard.
//!
//! The browser terminal (xterm.js over a PTY) is the live-screen surface; this
//! loop no longer pushes a `screen` mirror to clients. It still `capture`s the
//! pane internally to hash for activity (last-activity) and orphan detection.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::db::Db;
use crate::events::EventBus;
use crate::session::{self as session_mod, Session};
use crate::web::AppState;
use crate::{backend, events};
use weaver_core::branch as branch_mod;
use weaver_core::config as core_config;
use weaver_core::tags;

const TICK: Duration = Duration::from_millis(1500);

/// The retention reaper runs at most once every this many monitor ticks
/// (~90s at the 1.5s tick) — archiving is heavyweight next to the per-tick work.
const REAP_EVERY_TICKS: u32 = 60;

/// Neither reap trigger fires while the session's last activity is this recent.
/// Archiving destroys the worktree, so a session that just moved is never
/// reaped — even when its tracking issue has closed.
const REAP_GRACE_SECS: i64 = 900;

pub async fn run(state: AppState) {
    let mut screen_hash: HashMap<String, u64> = HashMap::new();
    // The session ids the monitor has already announced `stale` for, so a
    // session that stays quiet is announced once (edge-detected), not every
    // tick. A session leaves the set the moment its activity advances; it is
    // pruned with `screen_hash` when the session disappears.
    let mut stale_seen: HashSet<String> = HashSet::new();
    // Watermark: process every event written after this id, then advance.
    let mut last_event = events::max_id(&state.db).await.unwrap_or(0);
    // Ticks since the retention reaper last ran (see [`REAP_EVERY_TICKS`]).
    let mut reap_tick: u32 = 0;
    tracing::info!(tick_ms = TICK.as_millis() as u64, "monitor loop started");

    loop {
        tokio::time::sleep(TICK).await;

        // 1. Consume any new event rows and reflect them on the relevant
        //    session / branch.
        match events::since(&state.db, last_event).await {
            Ok(new_events) => {
                for ev in new_events {
                    last_event = last_event.max(ev.id);
                    match ev.kind.as_str() {
                        // A `tag` write — `weaver status` (the agent's
                        // `attention`), a watch's `triage`, or any free-form
                        // key — or an `artifact_written` from `weaver artifact
                        // write`: recorded daemon-less by the CLI, so it never
                        // touched the bus. Re-broadcast so live dashboards refresh
                        // the badge, pill, or artifact list; nothing else to do.
                        "tag" | "artifact_written" => {
                            state.bus.publish(ev.clone());
                            continue;
                        }
                        "hook" => {}
                        _ => continue,
                    }
                    let kind = ev
                        .data
                        .get("event")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if kind.is_empty() {
                        continue;
                    }
                    last_event = apply_hook(&state, &ev.branch_id, &kind)
                        .await
                        .unwrap_or(last_event);
                }
            }
            Err(e) => tracing::warn!("monitor: reading new events failed: {e}"),
        }

        // 2. Walk every session, check terminal liveness, do stillness detection.
        let sessions = match session_mod::list(&state.db).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("monitor: listing sessions failed: {e}");
                continue;
            }
        };
        let mut alive: HashSet<String> = HashSet::new();
        tracing::debug!(sessions = sessions.len(), "monitor tick: session walk");

        // Edge-detect no-activity staleness once per walk, gated on the
        // watch master switch (no consumer ⇒ no point emitting). The
        // threshold and `now` are read once and shared across the walk.
        let stale_enabled = core_config::get_bool(
            &state.db,
            "watch.enabled",
            core_config::DEFAULT_WATCH_ENABLED,
        )
        .await;
        let stale_after = core_config::get(&state.db, "watch.stale_after_secs")
            .await
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(core_config::DEFAULT_WATCH_STALE_AFTER_SECS);
        let now = Utc::now();

        for session in &sessions {
            alive.insert(session.id.clone());
            if session_mod::is_terminal(&session.status) {
                continue;
            }

            // Staleness: emit `stale` exactly on the not-stale → stale edge.
            if stale_enabled {
                last_event = detect_stale(
                    &state,
                    session,
                    stale_after,
                    now,
                    &mut stale_seen,
                    last_event,
                )
                .await;
            }
            if !backend::has_session(&session.term_session).await {
                if session.status == "orphaned" {
                    continue;
                }
                match session_mod::mark_orphaned(&state.db, &session.id).await {
                    Ok(true) => {
                        tracing::info!(
                            id = %session.id,
                            term_session = %session.term_session,
                            "terminal session ended; marked orphaned"
                        );
                        let _ = events::record(
                            &state.db,
                            &state.bus,
                            &session.branch_id,
                            "status",
                            json!({ "status": "orphaned", "reason": "terminal session ended" }),
                        )
                        .await;
                        last_event = events::max_id(&state.db).await.unwrap_or(last_event);
                    }
                    Ok(false) => tracing::debug!(
                        id = %session.id,
                        snapshot_status = %session.status,
                        "session no longer eligible for orphan transition"
                    ),
                    Err(e) => tracing::warn!(
                        id = %session.id,
                        error = %e,
                        "could not mark ended terminal session orphaned"
                    ),
                }
                continue;
            }

            // An ACP (relay) session has no vt100 screen to hash — its activity is
            // the turn boundary, bumped by `record_acp_lifecycle`. Skip the capture.
            if session.protocol == "acp" {
                continue;
            }
            // Hash the pane to detect activity and bump `last_activity_at`.
            // Inferred working→idle demotion is gone: liveness is all we can
            // know, and the agent reports the rest via `weaver status`.
            let screen = backend::capture(&session.term_session, 0)
                .await
                .unwrap_or_default();
            let h = hash(&normalize_screen(&screen));
            if screen_hash.get(&session.id) != Some(&h) {
                screen_hash.insert(session.id.clone(), h);
                tracing::debug!(id = %session.id, "activity detected; touching session");
                let _ = session_mod::touch(&state.db, &session.id).await;
            }
        }

        screen_hash.retain(|k, _| alive.contains(k));
        stale_seen.retain(|k| alive.contains(k));

        // 3. Retention: reap finished / long-idle automation sessions, on a
        //    slower cadence than the tick.
        reap_tick += 1;
        if reap_tick >= REAP_EVERY_TICKS {
            reap_tick = 0;
            reap_automation(&state, &sessions, now).await;
        }
    }
}

/// Why the reaper archives an automation session — [`reap_decision`]'s verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapReason {
    /// The tracking issue the session was launched for has been closed.
    IssueClosed,
    /// The session sat idle past `automation.idle_archive_secs`.
    IdleTtl,
}

/// Whether the reaper may consider `session` at all: automation-class, not a
/// warm (watch-managed) session — those are exempt infrastructure — and in a
/// non-terminal status.
fn reap_candidate(session: &Session) -> bool {
    session.class == "automation"
        && session.managed_by.is_none()
        && matches!(session.status.as_str(), "created" | "running" | "orphaned")
}

/// The pure reaper verdict for one session. `issue_closed` is the pre-fetched
/// status of the session's tracking issue (false when it has none), and
/// `idle_archive_secs <= 0` disables the TTL trigger. Both triggers share a
/// safety guard: no live ACP turn (`acp_inflight`; a terminal session's only
/// liveness signal is `last_activity_at` itself) and at least
/// [`REAP_GRACE_SECS`] of stillness — archiving destroys the worktree, so a
/// session that just moved is never reaped.
fn reap_decision(
    session: &Session,
    issue_closed: bool,
    idle_archive_secs: i64,
    now: DateTime<Utc>,
) -> Option<ReapReason> {
    if !reap_candidate(session) || session.acp_inflight.is_some() {
        return None;
    }
    let idle = idle_secs(session, now);
    if idle < REAP_GRACE_SECS {
        return None;
    }
    if issue_closed {
        return Some(ReapReason::IssueClosed);
    }
    if idle_archive_secs > 0 && idle >= idle_archive_secs {
        return Some(ReapReason::IdleTtl);
    }
    None
}

/// One reaper pass: archive every automation session [`reap_decision`] convicts,
/// via the shared archive path (worktree teardown + transcript capture + the
/// `status` event that lands on SSE). Errors are logged, never fatal to a tick.
async fn reap_automation(state: &AppState, sessions: &[Session], now: DateTime<Utc>) {
    for session in sessions {
        // Only a candidate's tracking issue is worth a lookup.
        if !reap_candidate(session) {
            continue;
        }
        let issue_closed = match session.tracking_issue_id {
            Some(issue_id) => match weaver_core::issue::get(&state.db, issue_id).await {
                Ok(issue) => issue.is_some_and(|i| i.status == "closed"),
                Err(e) => {
                    tracing::warn!(id = %session.id, issue = issue_id, error = %e,
                        "reaper: tracking issue lookup failed");
                    false
                }
            },
            None => false,
        };
        let ttl = session.policy_idle_archive_secs.unwrap_or(0);
        let Some(reason) = reap_decision(session, issue_closed, ttl, now) else {
            continue;
        };
        let branch = match branch_mod::get(&state.db, &session.branch_id).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                tracing::warn!(id = %session.id, branch = %session.branch_id,
                    "reaper: session's branch row is missing");
                continue;
            }
            Err(e) => {
                tracing::warn!(id = %session.id, error = %e, "reaper: branch lookup failed");
                continue;
            }
        };
        tracing::info!(id = %session.id, branch = %branch.branch, ?reason,
            "reaping automation session");
        if let Err(e) = crate::web::archive(state, session, &branch).await {
            tracing::warn!(id = %session.id, error = %e.message(), "reaper: archive failed");
        }
    }
}

/// The time a session was last active: its `last_activity_at`, or its
/// `created_at` for a session that has never been touched. `None` when neither
/// timestamp parses (a corrupt row treated as "no anchor" rather than panicking).
fn activity_anchor(session: &Session) -> Option<DateTime<Utc>> {
    session
        .last_activity_at
        .as_deref()
        .or(Some(session.created_at.as_str()))
        .and_then(parse_iso)
}

/// Whether `session` has been idle for at least `after` seconds as of `now`.
///
/// A non-positive threshold means "stale immediately" — useful for tests and a
/// deliberate operator setting. A session with no recorded `last_activity_at`
/// (never touched) falls back to its `created_at`, so a session that was created
/// and never moved still goes stale.
pub fn is_stale(session: &Session, after: i64, now: DateTime<Utc>) -> bool {
    let Some(anchor) = activity_anchor(session) else {
        return false;
    };
    (now - anchor).num_seconds() >= after
}

/// Emit a one-shot `stale` event on the not-stale → stale transition for one
/// session, edge-detected against `seen`. Returns the (possibly advanced) event
/// watermark so the monitor's own emission isn't reprocessed.
///
/// * Crosses into stale and not yet announced → record a branch-scoped `stale`
///   event (so a reactive trigger can resolve its repo) and remember the id.
/// * No longer stale (activity resumed) → forget the id, re-arming the edge.
///
/// Branch-scoped rather than system-scoped: the event carries the session's
/// branch so the dispatcher (`event_repo`) can repo-filter it.
pub async fn detect_stale(
    state: &AppState,
    session: &Session,
    after: i64,
    now: DateTime<Utc>,
    seen: &mut HashSet<String>,
    last_event: i64,
) -> i64 {
    if is_stale(session, after, now) {
        if seen.insert(session.id.clone()) {
            let idle_secs = idle_secs(session, now);
            tracing::info!(id = %session.id, idle_secs, "session marked stale");
            if events::record(
                &state.db,
                &state.bus,
                &session.branch_id,
                "stale",
                json!({ "session": session.id, "idle_secs": idle_secs }),
            )
            .await
            .is_ok()
            {
                return events::max_id(&state.db).await.unwrap_or(last_event);
            }
        }
    } else {
        // Activity resumed (or never crossed): re-arm the edge.
        if seen.remove(&session.id) {
            tracing::info!(id = %session.id, "session activity resumed; no longer stale");
        }
    }
    last_event
}

/// Seconds since the session's last activity (or creation), clamped at 0.
fn idle_secs(session: &Session, now: DateTime<Utc>) -> i64 {
    activity_anchor(session)
        .map(|t| (now - t).num_seconds().max(0))
        .unwrap_or(0)
}

/// Parse an ISO-8601 timestamp (the `weaver_core::db::now_iso` format) to UTC.
fn parse_iso(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Reflect a Claude lifecycle hook (`working` / `waiting` / `idle`) onto the
/// active session and its branch, broadcasting only what actually changed.
/// Returns the new event watermark (it records its own bus events). `None` when
/// there is no active session for the branch.
///
/// Mapping rationale: the work hooks drive only liveness and a soothing idle
/// signal. A `working` / `waiting` / `idle` hook means the agent process is
/// alive → `running` (this also lifts a recovered `orphaned` session back to
/// `running`).
/// `session-start` is returned early below — it is recorded for the primer
/// injection (in the `weaver hook` CLI) but the launch path owns the initial
/// status, so it carries no liveness or tag signal here. Beyond liveness:
///
/// * `working` (a prompt was submitted — the user is engaged) clears the calm
///   `idle` mark *and* the agent's `attention` tag back to calm: an engaged
///   agent is neither resting nor waiting on the user.
/// * `waiting` (a `Notification` lull) and `idle` (a turn ended) stamp the quiet
///   [`tags::IDLE_KEY`] mark — the soothing "resting, no one needed" state.
///   Crucially this is **not** loud, so a finished-but-fine agent no longer
///   reads as needing the user. They leave the agent's own `attention` tag
///   untouched (a loud self-report still wins the badge), and the status watch
///   may later replace this idle mark with a real loud status — or clear it —
///   once it judges the session genuinely needs a human.
///
/// We don't try to mechanically tell "truly idle" from "waiting on a sub-agent
/// or shell": the finished-turn hook is a good-enough idle signal, and the
/// status watch upgrades it when warranted.
async fn apply_hook(state: &AppState, branch_id: &str, kind: &str) -> Option<i64> {
    // Only the work-cycle hooks carry a status/tag signal here; `session-start`
    // and any unknown kind return early (they neither prove liveness nor mark a
    // tag).
    lifecycle_mutations(kind)?;
    let session = session_mod::active_for_branch(&state.db, branch_id)
        .await
        .ok()??;

    // Belt-and-braces: an ACP session's working/idle edges come from the protocol
    // (the acp task drives them via `record_acp_lifecycle`), so a work-cycle hook
    // a user's own `.claude/settings.local.json` might still fire must NOT move an
    // ACP session's status or idle mark. Terminal sessions promote as before.
    if session.protocol == "acp" {
        return None;
    }
    promote_lifecycle(&state.db, &state.bus, &session, kind).await
}

/// The tag mutations a work-cycle hook implies: `(key, value)` where an empty
/// value clears the tag (absence is the calm/default state). `working` returns
/// the agent to calm (clearing both axes it might carry); the quiet signals stamp
/// the soothing `idle` mark. `None` for a kind that carries no work-cycle signal.
fn lifecycle_mutations(kind: &str) -> Option<&'static [(&'static str, &'static str)]> {
    const WORKING: &[(&str, &str)] = &[(tags::ATTENTION_KEY, ""), (tags::IDLE_KEY, "")];
    const RESTING: &[(&str, &str)] = &[(tags::IDLE_KEY, tags::IDLE_VALUE)];
    match kind {
        "working" => Some(WORKING),
        "waiting" | "idle" => Some(RESTING),
        _ => None,
    }
}

/// Reflect a work-cycle lifecycle edge (`working`/`waiting`/`idle`) onto `session`
/// and its branch: lift the status to `running` (idempotent, never overriding a
/// terminal state) and apply the tag mutations, recording only what actually
/// changed. Returns the new event watermark.
async fn promote_lifecycle(db: &Db, bus: &EventBus, session: &Session, kind: &str) -> Option<i64> {
    let mutations = lifecycle_mutations(kind)?;
    let branch_id = session.branch_id.as_str();

    // Turn accounting: every `working` edge is one agent turn, counted for every
    // session. The cap applies only to automation-class sessions that are not
    // warm (watch-managed) infrastructure; past it, the branch is marked
    // `blocked` below instead of returning the attention axis to calm.
    let mut cap_note: Option<String> = None;
    if kind == "working" {
        match session_mod::increment_turn_count(db, &session.id).await {
            Ok(count) => {
                let cap = session.policy_turn_budget;
                if cap > 0
                    && count > cap
                    && session.class == "automation"
                    && session.managed_by.is_none()
                {
                    cap_note = Some(format!("turn cap ({cap}) reached"));
                }
            }
            Err(e) => {
                tracing::warn!(id = %session.id, error = %e, "turn count increment failed")
            }
        }
    }

    // Lifecycle: alive → running. Idempotent once running; never overrides a
    // terminal state.
    let status_changed = session.status != "running" && !session_mod::is_terminal(&session.status);
    if status_changed {
        if session.status == "orphaned" {
            tracing::info!(id = %session.id, branch = %branch_id, "lifting orphaned session back to running");
        } else {
            tracing::debug!(
                id = %session.id,
                branch = %branch_id,
                previous_status = %session.status,
                "session transitioning to running via lifecycle edge"
            );
        }
        let _ = session_mod::set_status(db, &session.id, "running").await;
    }
    let _ = session_mod::touch(db, &session.id).await;

    if status_changed {
        let _ = events::record(
            db,
            bus,
            branch_id,
            "status",
            json!({ "status": "running", "source": "hook" }),
        )
        .await;
    }

    // Apply each tag mutation only when it actually changes the stored value, so
    // a repeated edge (e.g. another finished turn while already idle) is a no-op
    // and dashboards refresh only on a real edge. The author is `agent` — these
    // are the agent's own lifecycle marks.
    for &(key, value) in mutations {
        // A capped session owns the attention axis below: don't let the routine
        // `working` calm-down clear the tag it is about to raise.
        if cap_note.is_some() && key == tags::ATTENTION_KEY {
            continue;
        }
        let current = tags::get(db, branch_id, key)
            .await
            .ok()
            .flatten()
            .map(|t| t.value)
            .unwrap_or_default();
        if current == value {
            continue;
        }
        tracing::debug!(branch = %branch_id, key, value, "lifecycle edge applied tag mutation");
        if value.is_empty() {
            let _ = tags::clear(db, branch_id, key).await;
        } else {
            let _ = tags::set(db, branch_id, key, value, "", "agent").await;
        }
        let _ = events::record_tag(db, bus, branch_id, key, value, "", "agent").await;
    }

    // Cap enforcement: past the cap an automation session reads as `blocked`, so
    // the fleet surfaces it instead of letting it burn turns quietly. Set once —
    // a repeated over-cap edge on an already-blocked branch is a no-op.
    if let Some(note) = cap_note {
        let already_blocked = tags::get(db, branch_id, tags::ATTENTION_KEY)
            .await
            .ok()
            .flatten()
            .is_some_and(|t| t.value == "blocked");
        if !already_blocked {
            tracing::info!(id = %session.id, branch = %branch_id, %note,
                "automation session over its turn cap; marking blocked");
            let _ = tags::set(
                db,
                branch_id,
                tags::ATTENTION_KEY,
                "blocked",
                &note,
                "agent",
            )
            .await;
            let _ = events::record_tag(
                db,
                bus,
                branch_id,
                tags::ATTENTION_KEY,
                "blocked",
                &note,
                "agent",
            )
            .await;
        }
    }

    // Advance the watermark past our own freshly-recorded events so the next
    // tick doesn't reprocess them. `None` on a read error just leaves the
    // caller's watermark untouched (the consumed event is already accounted for).
    events::max_id(db).await.ok()
}

/// Drive an ACP session's status/idle from a turn boundary — the acp task calls
/// this at turn start (`kind = "working"`) and turn end (`kind = "idle"`). It
/// records the same `hook` event row `weaver hook --event <kind>` would (the
/// durable audit trail), then promotes the status/tags directly through the
/// shared [`promote_lifecycle`] path — bypassing [`apply_hook`]'s ACP filter,
/// which exists only to ignore stray user-authored work-cycle hooks. Best-effort:
/// a missing session or write error is logged upstream, never fatal to the turn.
pub async fn record_acp_lifecycle(db: &Db, bus: &EventBus, session_id: &str, kind: &str) {
    let session = match session_mod::get(db, session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "acp lifecycle: session lookup failed");
            return;
        }
    };
    if let Err(e) =
        events::record_local(db, &session.branch_id, "hook", json!({ "event": kind })).await
    {
        tracing::warn!(session = %session_id, error = %e, "acp lifecycle: hook audit write failed");
    }
    let _ = promote_lifecycle(db, bus, &session, kind).await;
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Normalize a captured pane for stillness hashing so that a *resize* — which
/// changes the captured row count and pads/re-wraps lines — does not read as a
/// content change. With browser-driven `window-size latest`, an attached
/// client's size drives the captured geometry; without this normalization every
/// fit/resize/tab-open/tab-close would flip the hash, reset `still_ticks`, and
/// prevent a genuinely-idle non-hook agent from ever being marked idle. We strip
/// trailing whitespace per line and drop trailing blank rows.
fn normalize_screen(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
    while matches!(lines.last(), Some(&"")) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{is_stale, normalize_screen, Session};
    use chrono::{Duration, Utc};

    /// A bare `Session` with the given `last_activity_at`; only the timestamps
    /// matter for staleness.
    fn session_with_activity(last_activity_at: Option<&str>, created_at: &str) -> Session {
        Session {
            id: "s1".to_string(),
            branch_id: "b1".to_string(),
            work_dir: String::new(),
            term_session: String::new(),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            last_activity_at: last_activity_at.map(str::to_string),
            created_at: created_at.to_string(),
            parent_branch_id: None,
            managed_by: None,
            created_by: None,
            park: None,
            sort_order: None,
            protocol: "terminal".to_string(),
            acp_session_id: None,
            acp_ack_seq: 0,
            acp_inflight: None,
            current_mode: None,
            pending_prompt: None,
            origin: "user".to_string(),
            class: "interactive".to_string(),
            turn_count: 0,
            tracking_issue_id: None,
            profile: "default".to_string(),
            launch_mode: "auto".to_string(),
            profile_revision: 1,
            policy_env_clear: false,
            policy_ambient_allowlist: "[]".to_string(),
            policy_idle_archive_secs: None,
            policy_turn_budget: 0,
            policy_prelude: "weaver".to_string(),
            policy_restricted: false,
            policy_allowed_tools: "[]".to_string(),
            creator_kind: "user".to_string(),
            creator_subject: "owner".to_string(),
            parent_session_id: None,
            automation_run_id: None,
        }
    }

    #[test]
    fn is_stale_crosses_the_threshold() {
        let now = Utc::now();
        let iso = |t: chrono::DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        // Active 10 minutes ago, threshold 30 minutes → not stale.
        let recent = session_with_activity(Some(&iso(now - Duration::minutes(10))), &iso(now));
        assert!(!is_stale(&recent, 1800, now));

        // Active 40 minutes ago, threshold 30 minutes → stale.
        let old = session_with_activity(Some(&iso(now - Duration::minutes(40))), &iso(now));
        assert!(is_stale(&old, 1800, now));

        // No recorded activity falls back to created_at.
        let never = session_with_activity(None, &iso(now - Duration::minutes(40)));
        assert!(is_stale(&never, 1800, now));

        // A zero threshold means "stale immediately" (the test/operator knob).
        assert!(is_stale(&recent, 0, now));

        // An unparseable timestamp is treated as not stale rather than panicking.
        let bad = session_with_activity(Some("not-a-time"), "also-bad");
        assert!(!is_stale(&bad, 0, now));
    }

    #[test]
    fn reap_decision_triggers_and_guards() {
        use super::{reap_decision, ReapReason, REAP_GRACE_SECS};
        let now = Utc::now();
        let iso = |t: chrono::DateTime<Utc>| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let long_ago = iso(now - Duration::hours(9));
        let ttl = 28800; // the 8h default

        let mut idle = session_with_activity(Some(&long_ago), &long_ago);
        idle.class = "automation".to_string();

        // Past the TTL → IdleTtl; a closed tracking issue wins even with the
        // TTL disabled; neither trigger without a closed issue and TTL off.
        assert_eq!(
            reap_decision(&idle, false, ttl, now),
            Some(ReapReason::IdleTtl)
        );
        assert_eq!(
            reap_decision(&idle, true, 0, now),
            Some(ReapReason::IssueClosed)
        );
        assert_eq!(reap_decision(&idle, false, 0, now), None);

        // Past the grace window but under the TTL: kept on the TTL axis, but a
        // closed tracking issue still reaps it.
        let mid = iso(now - Duration::seconds(REAP_GRACE_SECS + 60));
        let mut settled = session_with_activity(Some(&mid), &mid);
        settled.class = "automation".to_string();
        assert_eq!(reap_decision(&settled, false, ttl, now), None);
        assert_eq!(
            reap_decision(&settled, true, ttl, now),
            Some(ReapReason::IssueClosed)
        );

        // The shared guard: recent activity or a live ACP turn blocks BOTH
        // triggers — archive destroys the worktree.
        let fresh_at = iso(now - Duration::minutes(5));
        let mut fresh = session_with_activity(Some(&fresh_at), &fresh_at);
        fresh.class = "automation".to_string();
        assert_eq!(reap_decision(&fresh, true, ttl, now), None);
        let mut inflight = idle.clone();
        inflight.acp_inflight = Some("{}".to_string());
        assert_eq!(reap_decision(&inflight, true, ttl, now), None);

        // Not a candidate: interactive class, warm (watch-managed), or already
        // in a terminal status.
        let mut interactive = idle.clone();
        interactive.class = "interactive".to_string();
        assert_eq!(reap_decision(&interactive, true, ttl, now), None);
        let mut warm = idle.clone();
        warm.managed_by = Some("w1".to_string());
        assert_eq!(reap_decision(&warm, true, ttl, now), None);
        let mut archived = idle.clone();
        archived.status = "archived".to_string();
        assert_eq!(reap_decision(&archived, true, ttl, now), None);
    }

    #[test]
    fn normalize_ignores_resize_padding() {
        // Same content, different captured geometry (extra blank rows + trailing
        // padding from a wider/taller client) must hash identically.
        let narrow = "bash-5.2$ ls\nfile.txt\nbash-5.2$";
        let wide = "bash-5.2$ ls   \nfile.txt        \nbash-5.2$\n\n\n";
        assert_eq!(normalize_screen(narrow), normalize_screen(wide));
    }

    #[test]
    fn normalize_keeps_real_changes() {
        let before = "bash-5.2$ ls\nfile.txt";
        let after = "bash-5.2$ ls\nfile.txt\nother.txt";
        assert_ne!(normalize_screen(before), normalize_screen(after));
    }

    // -- apply_hook / lifecycle promotion ----------------------------------

    use crate::session::{self as session_mod, NewSession};
    use crate::web::AppState;
    use weaver_core::branch as branch_mod;
    use weaver_core::tags;

    fn test_state(db: crate::db::Db) -> AppState {
        AppState {
            db: db.clone(),
            bus: crate::events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db),
            acp: crate::acp::AcpRegistry::new(),
        }
    }

    async fn seed_session(
        db: &crate::db::Db,
        id: &str,
        branch_name: &str,
        protocol: &str,
    ) -> String {
        let branch = branch_mod::upsert(db, "/r", branch_name, "main")
            .await
            .unwrap();
        session_mod::insert(
            db,
            &NewSession {
                id: id.to_string(),
                branch_id: branch.id.clone(),
                work_dir: "/w".to_string(),
                term_session: format!("weaver-{id}"),
                agent_kind: "claude".to_string(),
                model: String::new(),
                effort: String::new(),
                // Orphaned is non-terminal, so `active_for_branch` resolves it and a
                // lifecycle edge would lift it to `running` — a visible signal.
                status: "orphaned".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: None,
                protocol: protocol.to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        branch.id
    }

    /// The work-cycle hook path promotes a terminal session (status lift + idle
    /// mark) but is a no-op for an ACP session — whose turn edges the protocol
    /// owns — even though both would resolve to the same active session.
    #[tokio::test]
    async fn apply_hook_ignores_acp_but_promotes_terminal() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let state = test_state(db.clone());
        let term_branch = seed_session(&db, "term1", "weaver/term", "terminal").await;
        let acp_branch = seed_session(&db, "acp1", "weaver/acp", "acp").await;

        // An `idle` edge on the terminal session: lifted to running, idle stamped.
        super::apply_hook(&state, &term_branch, "idle").await;
        let ts = session_mod::get(&db, "term1").await.unwrap().unwrap();
        assert_eq!(ts.status, "running", "terminal session lifted to running");
        assert_eq!(
            tags::get(&db, &term_branch, tags::IDLE_KEY)
                .await
                .unwrap()
                .map(|t| t.value)
                .as_deref(),
            Some(tags::IDLE_VALUE),
            "terminal session's idle mark stamped"
        );

        // The same edge on the ACP session: ignored entirely.
        super::apply_hook(&state, &acp_branch, "idle").await;
        let as_ = session_mod::get(&db, "acp1").await.unwrap().unwrap();
        assert_eq!(
            as_.status, "orphaned",
            "acp session status untouched by hook"
        );
        assert!(
            tags::get(&db, &acp_branch, tags::IDLE_KEY)
                .await
                .unwrap()
                .is_none(),
            "acp session's idle mark NOT stamped by the hook path"
        );

        // The direct acp lifecycle entry DOES promote it (the acp task's path).
        super::record_acp_lifecycle(&db, &state.bus, "acp1", "idle").await;
        let as2 = session_mod::get(&db, "acp1").await.unwrap().unwrap();
        assert_eq!(
            as2.status, "running",
            "record_acp_lifecycle lifts the acp session"
        );
        assert_eq!(
            tags::get(&db, &acp_branch, tags::IDLE_KEY)
                .await
                .unwrap()
                .map(|t| t.value)
                .as_deref(),
            Some(tags::IDLE_VALUE),
            "record_acp_lifecycle stamps the idle mark directly"
        );
    }
}
