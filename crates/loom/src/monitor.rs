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

use crate::session::{self as session_mod, Session};
use crate::web::AppState;
use crate::{backend, events};
use weaver_core::config as core_config;
use weaver_core::tags;

const TICK: Duration = Duration::from_millis(1500);

pub async fn run(state: AppState) {
    let mut screen_hash: HashMap<String, u64> = HashMap::new();
    // The session ids the monitor has already announced `stale` for, so a
    // session that stays quiet is announced once (edge-detected), not every
    // tick. A session leaves the set the moment its activity advances; it is
    // pruned with `screen_hash` when the session disappears.
    let mut stale_seen: HashSet<String> = HashSet::new();
    // Watermark: process every event written after this id, then advance.
    let mut last_event = events::max_id(&state.db).await.unwrap_or(0);
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
                if session.status != "orphaned" {
                    tracing::info!(
                        id = %session.id,
                        term_session = %session.term_session,
                        "terminal session ended; marking orphaned"
                    );
                    let _ = session_mod::set_status(&state.db, &session.id, "orphaned").await;
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
    // The tag mutations the hook implies: `(key, value)` where an empty value
    // clears the tag (absence is the calm/default state). `working` returns the
    // agent to calm (clearing both axes it might carry); the quiet signals stamp
    // the soothing `idle` mark. `session-start` and any unknown kind return early
    // — they neither prove liveness here nor carry a tag signal.
    let mutations: &[(&str, &str)] = match kind {
        "working" => &[(tags::ATTENTION_KEY, ""), (tags::IDLE_KEY, "")],
        "waiting" | "idle" => &[(tags::IDLE_KEY, tags::IDLE_VALUE)],
        _ => return None,
    };

    let session = session_mod::active_for_branch(&state.db, branch_id)
        .await
        .ok()??;

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
                "session transitioning to running via hook"
            );
        }
        let _ = session_mod::set_status(&state.db, &session.id, "running").await;
    }
    let _ = session_mod::touch(&state.db, &session.id).await;

    if status_changed {
        let _ = events::record(
            &state.db,
            &state.bus,
            branch_id,
            "status",
            json!({ "status": "running", "source": "hook" }),
        )
        .await;
    }

    // Apply each tag mutation only when it actually changes the stored value, so
    // a repeated hook (e.g. another finished turn while already idle) is a no-op
    // and dashboards refresh only on a real edge. The author is `agent` — these
    // are the agent's own lifecycle marks.
    for &(key, value) in mutations {
        let current = tags::get(&state.db, branch_id, key)
            .await
            .ok()
            .flatten()
            .map(|t| t.value)
            .unwrap_or_default();
        if current == value {
            continue;
        }
        tracing::debug!(branch = %branch_id, key, value, "hook applied tag mutation");
        if value.is_empty() {
            let _ = tags::clear(&state.db, branch_id, key).await;
        } else {
            let _ = tags::set(&state.db, branch_id, key, value, "", "agent").await;
        }
        let _ = events::record_tag(&state.db, &state.bus, branch_id, key, value, "", "agent").await;
    }

    // Advance the watermark past our own freshly-recorded events so the next
    // tick doesn't reprocess them. `None` on a read error just leaves the
    // caller's watermark untouched (the consumed event is already accounted for).
    events::max_id(&state.db).await.ok()
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
}
