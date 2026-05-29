//! Background task: detects when a session's tmux has ended, drives
//! screen-stillness idle detection, and consumes `hook` events written by the
//! `weaver hook` CLI to update session status.
//!
//! The browser terminal (xterm.js over a PTY) is the live-screen surface; this
//! loop no longer pushes a `screen` mirror to clients. It still `capture`s the
//! pane internally to hash for stillness/idle/orphan detection.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;

use crate::session as session_mod;
use crate::web::AppState;
use crate::{events, tmux};

const TICK: Duration = Duration::from_millis(1500);
const IDLE_TICKS: u32 = 10;

pub async fn run(state: AppState) {
    let mut screen_hash: HashMap<String, u64> = HashMap::new();
    let mut still_ticks: HashMap<String, u32> = HashMap::new();
    // Watermark: process every event written after this id, then advance.
    let mut last_event = events::max_id(&state.db).await.unwrap_or(0);
    tracing::info!(tick_ms = TICK.as_millis() as u64, "monitor loop started");

    loop {
        tokio::time::sleep(TICK).await;

        // 1. Consume any new event rows (hooks, etc.) and reflect them on the
        //    relevant session.
        match events::since(&state.db, last_event).await {
            Ok(new_events) => {
                for ev in new_events {
                    last_event = last_event.max(ev.id);
                    if ev.kind != "hook" {
                        continue;
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
                    // `session-start` fires only to inject the primer via
                    // `additionalContext`; it intentionally does not change
                    // session status (otherwise a resume would flip an idle
                    // session to working with no user action).
                    let status = match kind.as_str() {
                        "working" => "working",
                        "waiting" => "waiting",
                        "idle" => "idle",
                        _ => continue,
                    };
                    if let Ok(Some(session)) =
                        session_mod::active_for_branch(&state.db, &ev.branch_id).await
                    {
                        let _ = session_mod::set_status(&state.db, &session.id, status).await;
                        let _ = session_mod::touch(&state.db, &session.id).await;
                        let prompt = if status == "waiting" {
                            tmux::capture(&session.tmux_session, 0)
                                .await
                                .map(|s| s.trim().to_string())
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        let _ =
                            session_mod::set_pending_prompt(&state.db, &session.id, &prompt).await;
                        let mut data = json!({ "status": status, "source": "hook" });
                        if !prompt.is_empty() {
                            data["prompt"] = json!(prompt);
                        }
                        let _ =
                            events::record(&state.db, &state.bus, &ev.branch_id, "status", data)
                                .await;
                        // Bump the watermark past our own freshly-recorded
                        // event so we don't loop on it.
                        last_event = events::max_id(&state.db).await.unwrap_or(last_event);
                    }
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

            let screen = tmux::capture(&session.tmux_session, 0)
                .await
                .unwrap_or_default();
            let h = hash(&normalize_screen(&screen));
            if screen_hash.get(&session.id) != Some(&h) {
                screen_hash.insert(session.id.clone(), h);
                still_ticks.insert(session.id.clone(), 0);
                let _ = session_mod::touch(&state.db, &session.id).await;
            } else {
                let ticks = still_ticks.entry(session.id.clone()).or_insert(0);
                *ticks += 1;
                if session.agent_kind != "claude"
                    && session.status == "working"
                    && *ticks >= IDLE_TICKS
                {
                    let _ = session_mod::set_status(&state.db, &session.id, "idle").await;
                    let _ = events::record(
                        &state.db,
                        &state.bus,
                        &session.branch_id,
                        "status",
                        json!({ "status": "idle", "source": "monitor" }),
                    )
                    .await;
                    last_event = events::max_id(&state.db).await.unwrap_or(last_event);
                }
            }
        }

        screen_hash.retain(|k, _| alive.contains(k));
        still_ticks.retain(|k, _| alive.contains(k));
    }
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
