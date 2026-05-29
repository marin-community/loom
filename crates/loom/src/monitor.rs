//! Background task: mirrors each session's tmux screen, detects when a
//! session has ended, drives screen-stillness idle detection, and consumes
//! `hook` events written by the `weaver hook` CLI to update session status.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;

use crate::web::AppState;
use crate::{events, tmux};
use crate::session as session_mod;

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
                        let _ = session_mod::set_pending_prompt(&state.db, &session.id, &prompt)
                            .await;
                        let mut data = json!({ "status": status, "source": "hook" });
                        if !prompt.is_empty() {
                            data["prompt"] = json!(prompt);
                        }
                        let _ = events::record(
                            &state.db,
                            &state.bus,
                            &ev.branch_id,
                            "status",
                            data,
                        )
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

            let screen = tmux::capture(&session.tmux_session, 0).await.unwrap_or_default();
            let h = hash(&screen);
            if screen_hash.get(&session.id) != Some(&h) {
                screen_hash.insert(session.id.clone(), h);
                still_ticks.insert(session.id.clone(), 0);
                let _ = session_mod::touch(&state.db, &session.id).await;
                events::emit(&state.bus, &session.branch_id, "screen", json!({ "content": screen }));
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
