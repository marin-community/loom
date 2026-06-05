//! Background task: detects when a session's tmux has ended and consumes the
//! event rows the `weaver` CLI writes — `hook` events (Claude lifecycle) and
//! `attention` events (`weaver status`) — reflecting them onto the session and
//! the dashboard.
//!
//! The browser terminal (xterm.js over a PTY) is the live-screen surface; this
//! loop no longer pushes a `screen` mirror to clients. It still `capture`s the
//! pane internally to hash for activity (last-activity) and orphan detection.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;

use crate::session as session_mod;
use crate::web::AppState;
use crate::{events, tmux};
use weaver_core::branch as branch_mod;

const TICK: Duration = Duration::from_millis(1500);

pub async fn run(state: AppState) {
    let mut screen_hash: HashMap<String, u64> = HashMap::new();
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
                        // `weaver status` wrote the branch's attention fields
                        // directly (daemon-less) but, going through the CLI,
                        // never touched the bus. Re-broadcast so live dashboards
                        // refresh; nothing else to do.
                        "attention" => {
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

        // 2. Walk every session, check tmux liveness, do stillness detection.
        let sessions = match session_mod::list(&state.db).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("monitor: listing sessions failed: {e}");
                continue;
            }
        };
        let mut alive: HashSet<String> = HashSet::new();

        for session in &sessions {
            alive.insert(session.id.clone());
            if session_mod::is_terminal(&session.status) {
                continue;
            }
            if !tmux::has_session(&session.tmux_session).await {
                if session.status != "orphaned" {
                    tracing::info!(
                        id = %session.id,
                        tmux_session = %session.tmux_session,
                        "tmux session ended; marking orphaned"
                    );
                    let _ = session_mod::set_status(&state.db, &session.id, "orphaned").await;
                    let _ = session_mod::set_pending_prompt(&state.db, &session.id, "").await;
                    let _ = events::record(
                        &state.db,
                        &state.bus,
                        &session.branch_id,
                        "status",
                        json!({ "status": "orphaned", "reason": "tmux session ended" }),
                    )
                    .await;
                    last_event = events::max_id(&state.db).await.unwrap_or(last_event);
                }
                continue;
            }

            // Hash the pane to detect activity and bump `last_activity_at`.
            // Inferred working→idle demotion is gone: liveness is all we can
            // know, and the agent reports the rest via `weaver status`.
            let screen = tmux::capture(&session.tmux_session, 0)
                .await
                .unwrap_or_default();
            let h = hash(&normalize_screen(&screen));
            if screen_hash.get(&session.id) != Some(&h) {
                screen_hash.insert(session.id.clone(), h);
                let _ = session_mod::touch(&state.db, &session.id).await;
            }
        }

        screen_hash.retain(|k, _| alive.contains(k));
    }
}

/// Reflect a Claude lifecycle hook (`working` / `waiting` / `idle`) onto the
/// active session and its branch, broadcasting only what actually changed.
/// Returns the new event watermark (it records its own bus events). `None` when
/// there is no active session for the branch.
///
/// Mapping rationale: hooks now drive only liveness and the genuine
/// attention signals. Any hook means the agent process is alive → `running`
/// (this also promotes a freshly-`launching` session). Beyond that:
///
/// * `working` (a prompt was submitted — the user is engaged) clears attention
///   back to `ok` and drops any pending prompt.
/// * `waiting` (Claude is blocked asking the user) raises attention to
///   `attention` and snapshots the pane as the pending prompt.
/// * `idle` (a turn ended) leaves attention untouched — a finished-but-fine
///   agent must not be mistaken for one that needs the user. If it actually
///   needs something it will have said so via `weaver status`.
async fn apply_hook(state: &AppState, branch_id: &str, kind: &str) -> Option<i64> {
    enum Prompt {
        Capture,
        Clear,
        Leave,
    }
    let (attention, prompt): (Option<(&str, &str)>, Prompt) = match kind {
        "working" => (Some(("ok", "")), Prompt::Clear),
        "waiting" => (Some(("attention", "Waiting for input")), Prompt::Capture),
        "idle" => (None, Prompt::Leave),
        // `session-start` and anything unknown carry no status signal.
        _ => return None,
    };

    let session = session_mod::active_for_branch(&state.db, branch_id)
        .await
        .ok()??;

    // Lifecycle: alive → running. Idempotent once running; never overrides a
    // terminal state.
    let status_changed = session.status != "running" && !session_mod::is_terminal(&session.status);
    if status_changed {
        let _ = session_mod::set_status(&state.db, &session.id, "running").await;
    }
    let _ = session_mod::touch(&state.db, &session.id).await;

    match prompt {
        Prompt::Capture => {
            let p = tmux::capture(&session.tmux_session, 0)
                .await
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let _ = session_mod::set_pending_prompt(&state.db, &session.id, &p).await;
        }
        Prompt::Clear => {
            let _ = session_mod::set_pending_prompt(&state.db, &session.id, "").await;
        }
        Prompt::Leave => {}
    }

    // Attention, only when the hook carries a signal and the value differs.
    let mut attention_changed: Option<(&str, &str)> = None;
    if let Some((level, note)) = attention {
        if let Ok(Some(branch)) = branch_mod::get(&state.db, branch_id).await {
            if branch.attention != level || branch.attention_note != note {
                let _ = branch_mod::set_attention(&state.db, branch_id, level, note).await;
                attention_changed = Some((level, note));
            }
        }
    }

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
    if let Some((level, note)) = attention_changed {
        let _ = events::record(
            &state.db,
            &state.bus,
            branch_id,
            "attention",
            json!({ "level": level, "note": note, "source": "hook" }),
        )
        .await;
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
    use super::normalize_screen;

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
