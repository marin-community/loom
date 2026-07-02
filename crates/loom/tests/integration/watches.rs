//! The Watch engine's **wiring**: the dispatcher's event→trigger
//! matching, rounds landing marks and audit rows against the live server, and
//! the guardrails (cooldown / no-overlap → `skipped`).
//!
//! Test placement: a builtin program's *decision logic* is covered
//! server-free by pytest (`python/weaver-loom/tests/`); these cases prove the
//! plumbing — the script runs under the engine, reaches the fleet over REST,
//! and its mutations land with attribution. Don't re-test program logic here.
//!
//! These cases drive the engine **directly** on the test server's isolated db
//! rather than through the spawned background loop: the test harness pins the
//! `watch.enabled` master switch off (it ships on by default), so the
//! daemon's own engine idles and never races these deterministic calls. Each
//! test builds its own
//! `AppState` over the same isolated db (the harness exports `WEAVER_HOME`) and
//! calls the public engine seams — `dispatch`, `fire_now`, `new_in_flight` /
//! `fire` — so a round runs without waiting on the timer.

use std::collections::HashSet;

use chrono::Utc;
use serde_json::json;
use serial_test::serial;

use loom::events::EventBus;
use loom::web::AppState;
use loom::{backend, db, events, monitor, server, session as session_mod, watch};
use weaver_core::config as core_config;
use weaver_core::watch as watch_store;

use crate::fixtures::{branch_tag, branch_tag_value, TestServer};

/// An `AppState` over the test server's isolated db — a second connection to the
/// same sqlite file (WAL, so concurrent readers/writers are fine). Lets a test
/// call the in-process engine seams the spawned loop would otherwise own.
async fn engine_state(ts: &TestServer) -> AppState {
    let pool = db::connect(&db::default_db_path()).await.unwrap();
    AppState {
        trigger: loom::github_trigger::GithubTrigger::production(pool.clone()),
        db: pool,
        bus: EventBus::new(),
        addr: ts.addr.to_string(),
        ide: std::sync::Arc::new(loom::ide::IdeManager::new(loom::ide::ide_home())),
    }
}

/// Create a `shell` session and return `(session_id, branch_id, repo_root)`.
async fn make_session(ts: &TestServer, goal: &str) -> (String, String, String) {
    let ws = ts
        .client
        .post(
            "/api/sessions",
            json!({ "goal": goal, "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let branch_id = ws["branch"]["id"].as_str().unwrap().to_string();
    let repo_root = ws["branch"]["repo_root"].as_str().unwrap().to_string();
    (id, branch_id, repo_root)
}

/// Register an enabled watch and return it.
async fn enabled_watch(state: &AppState, new: watch_store::NewWatch) -> watch_store::Watch {
    let o = watch_store::create(&state.db, &new).await.unwrap();
    watch_store::set_enabled(&state.db, &o.id, true)
        .await
        .unwrap();
    watch_store::get(&state.db, &o.id).await.unwrap().unwrap()
}

/// The test-owned **survey fixture program** (`programs/survey.py`): records
/// one `survey` action per surveyed session and mutates nothing. Engine-
/// mechanics tests run it so they can assert "a round ran over exactly these
/// sessions" from the run row alone, with no dependency on any builtin
/// program's behavior (that logic is pytest-owned).
fn survey_program() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/integration/programs/survey.py"
    )
    .to_string()
}

/// A reactive fixture that declares a `pr.merged` subscription and surveys only
/// the triggering session (via `triggered_sessions`).
fn survey_triggered_program() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/integration/programs/survey_triggered.py"
    )
    .to_string()
}

/// The session ids a run's recorded `survey` actions name.
fn surveyed_ids(run: &watch_store::WatchRun) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(&run.actions)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|a| a["action"] == "survey")
        .filter_map(|a| a["session"].as_str().map(str::to_string))
        .collect()
}

/// T4: a reactive event whose trigger matches fires the right watch exactly
/// once and lands a run row; a repo filter excludes another repo's event; and
/// re-firing the same event is idempotent (a fresh re-survey, never a second
/// "handling" of the event).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatcher_matches_trigger_with_repo_filter_and_is_idempotent() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, branch_id, repo_root) = make_session(&ts, "watch me").await;

    // The agent declares `blocked` about itself (the reactive signal): its own
    // `attention` tag.
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "blocked", "by": "agent" }),
        )
        .await
        .unwrap();

    // A watch reacting to `attention` events in *this* repo, surveying
    // the scoped (non-ok) fleet via the test-owned fixture program.
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "blocked-watch".to_string(),
            trigger_spec: json!({ "event": "attention", "repo": repo_root }).to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: survey_program(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;

    let in_flight = watch::new_in_flight();

    // A matching reactive event: a `tag` write of the `attention` tag (value
    // `blocked`) on a branch in this repo. The dispatcher maps the tag's
    // key/value onto the trigger's match kind/level.
    let ev = events::Event {
        id: 0,
        branch_id: branch_id.clone(),
        kind: "tag".to_string(),
        data: json!({ "key": "attention", "value": "blocked" }),
        created_at: db::now_iso(),
    };
    watch::dispatch(&state, &in_flight, &ev).await;

    // Exactly one run, and its run row records the survey of the in-scope
    // session — the engine-observable proof the round ran.
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1, "one matching event fires exactly one round");
    assert_eq!(runs[0].outcome, "ok");
    assert_eq!(
        surveyed_ids(&runs[0]),
        vec![session_id.clone()],
        "the round surveyed exactly the in-scope session"
    );

    // Re-firing the identical event is idempotent: level-triggered, the round
    // just re-surveys the current fleet — a fresh run row, same survey.
    watch::dispatch(&state, &in_flight, &ev).await;
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 2, "a re-fire is a fresh round");
    assert_eq!(
        surveyed_ids(&runs[0]),
        vec![session_id.clone()],
        "the re-fired round re-surveys, it does not 'handle' the event again"
    );

    // The repo filter excludes an event from another repo: same kind/level but a
    // branch that lives elsewhere must not fire this watch.
    let runs_before = watch_store::recent_runs(&state.db, &o.id, 100)
        .await
        .unwrap()
        .len();
    let other_repo_event = events::Event {
        id: 0,
        // A system (branchless) event carries no repo, so the repo-filtered
        // trigger must not match it.
        branch_id: events::SYSTEM_BRANCH.to_string(),
        kind: "tag".to_string(),
        data: json!({ "key": "attention", "value": "blocked" }),
        created_at: db::now_iso(),
    };
    watch::dispatch(&state, &in_flight, &other_repo_event).await;
    let runs_after = watch_store::recent_runs(&state.db, &o.id, 100)
        .await
        .unwrap()
        .len();
    assert_eq!(
        runs_before, runs_after,
        "a repo-filtered trigger ignores an event from outside its repo"
    );

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// T7: a session that crosses the staleness threshold causes the monitor to emit
/// a one-shot `stale` event, a watch with an `{"event":"stale"}` trigger
/// fires a round off it, and the emission is edge-detected — driving the
/// staleness check twice produces exactly one `stale` row, not one per pass.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_session_emits_one_event_and_wakes_a_reactive_watch() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "go stale").await;

    // The session reports `attention` about itself so it falls inside the
    // watch's `!ok` scope (the survey will name it, proving it ran).
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();

    // A reactive watch matching `stale` events fleet-wide.
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "stale-watch".to_string(),
            trigger_spec: json!({ "event": "stale" }).to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: survey_program(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;

    // Drive the monitor's staleness check directly with a zero threshold (stale
    // immediately) — the deterministic, fast stand-in for waiting 30 minutes.
    let session = session_mod::get(&state.db, &session_id)
        .await
        .unwrap()
        .unwrap();
    let watermark = events::max_id(&state.db).await.unwrap();
    let mut seen: HashSet<String> = HashSet::new();
    let now = Utc::now();

    // Two passes: the edge fires on the first, the second is a no-op.
    monitor::detect_stale(&state, &session, 0, now, &mut seen, watermark).await;
    monitor::detect_stale(&state, &session, 0, now, &mut seen, watermark).await;

    // Exactly one `stale` event was emitted across both passes (edge-detected).
    let new_events = events::since(&state.db, watermark).await.unwrap();
    let stale: Vec<_> = new_events.iter().filter(|e| e.kind == "stale").collect();
    assert_eq!(
        stale.len(),
        1,
        "the monitor emits `stale` once per transition, not every tick: {new_events:?}"
    );
    let stale_ev = stale[0];
    assert_eq!(
        stale_ev.data["session"],
        session_id.as_str(),
        "the stale event names the idle session"
    );
    assert!(
        stale_ev.data["idle_secs"].is_number(),
        "the stale event carries an idle_secs"
    );
    assert!(
        !events::is_system(&stale_ev.branch_id),
        "a stale event is branch-scoped so its repo resolves for repo-filtering"
    );

    // The dispatcher consumes the stale tick and fires the reactive round; the
    // run row records the survey of the in-scope stale session.
    let in_flight = watch::new_in_flight();
    watch::dispatch(&state, &in_flight, stale_ev).await;
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1, "the stale event fires exactly one round");
    assert_eq!(
        surveyed_ids(&runs[0]),
        vec![session_id.clone()],
        "the woken round surveyed the in-scope stale session"
    );

    // Once activity resumes (a non-stale pass clears the session from `seen`),
    // the edge re-arms: a later stale crossing emits a fresh event.
    let watermark2 = events::max_id(&state.db).await.unwrap();
    // A high threshold makes this just-touched session read as not-stale, which
    // re-arms the edge…
    monitor::detect_stale(&state, &session, 100_000, now, &mut seen, watermark2).await;
    assert!(
        !seen.contains(&session_id),
        "a not-stale pass clears the session, re-arming the edge"
    );
    // …so crossing the threshold again emits a second `stale` event.
    monitor::detect_stale(&state, &session, 0, now, &mut seen, watermark2).await;
    let after = events::since(&state.db, watermark2).await.unwrap();
    assert_eq!(
        after.iter().filter(|e| e.kind == "stale").count(),
        1,
        "after the edge re-arms, a new crossing emits again"
    );

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// Write a one-shot fake judge agent that ignores its stdin (the composed
/// prompt) and prints `out`, point `WEAVER_WATCH_AGENT_CMD` at it, and
/// return its path. Robust where `cat` is not: the round feeds a real terminal
/// screen into the prompt, and a shell screen can carry brackets that would
/// corrupt an echo-then-parse. Reused paths overwrite, so a test can re-stub
/// between fires. Restore `WEAVER_WATCH_AGENT_CMD=true` after.
fn fake_judge_agent(name: &str, out: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(name);
    let script = format!("#!/bin/sh\ncat >/dev/null 2>&1\ncat <<'WEAVER_EOF'\n{out}\nWEAVER_EOF\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("WEAVER_WATCH_AGENT_CMD", path.to_str().unwrap());
    path
}

/// The status program's wiring proof: an idle-triggered round asks the judge for
/// a set of tags and reconciles its own marks — a recommended tag lands as its
/// own typed key (never the agent's `attention` axis — no mirror), and a
/// follow-up "nothing needed" verdict clears it. The program's decision logic
/// (parse, no-judgement vs calm, capability branches, summaries) is covered
/// server-free in `python/weaver-loom/tests/test_status_program.py`; dry-run
/// flag propagation through the executor is covered by the lib test
/// `run_script_round_trips_the_contract`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_status_round_applies_typed_tags_and_reconciles() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "status me").await;

    // The judge model is stubbed: a fixed tag set the round should apply.
    let _agent = fake_judge_agent(
        "weaver-fake-judge-status",
        r#"[{"key":"review","value":"attention","note":"looks done"}]"#,
    );

    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "status-check".to_string(),
            trigger_spec: json!({ "on": ["session.idle"] }).to_string(),
            scope: json!({}).to_string(),
            program: "builtin:status".to_string(),
            capabilities: vec!["observe".to_string(), "mark".to_string()],
            ..Default::default()
        },
    )
    .await;

    // A manual round (run-now) fires the stock program directly.
    let run_id = watch::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();

    // The judged tag landed on its own typed key, attributed to the watch.
    let view = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(
        branch_tag_value(&view, "review"),
        "attention",
        "the round applies the judge's typed tag"
    );
    assert_eq!(
        branch_tag(&view, "review").unwrap()["set_by"],
        "status-check"
    );
    assert!(
        branch_tag(&view, "attention").is_none(),
        "the watch never mirrors onto the agent's own `attention` axis"
    );

    // The run row records a `tag` action (not the old generic `mark`).
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    let run = runs.iter().find(|r| r.id == run_id).unwrap();
    assert_eq!(run.outcome, "ok");
    let actions: serde_json::Value = serde_json::from_str(&run.actions).unwrap();
    let arr = actions.as_array().unwrap();
    assert!(
        arr.iter().any(|a| a["action"] == "tag"
            && a["session"] == session_id.as_str()
            && a["key"] == "review"),
        "the run records a tag action: {actions}"
    );

    // Reconcile: a follow-up "nothing needed" verdict clears the watch's own mark.
    let _calm = fake_judge_agent("weaver-fake-judge-status", "[]");
    watch::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();
    let view = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert!(
        branch_tag(&view, "review").is_none(),
        "the empty verdict clears the watch's own mark"
    );

    // Restore the fixture's no-op agent for the tests that follow.
    std::env::set_var("WEAVER_WATCH_AGENT_CMD", "true");
    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// The `resume` builtin's wiring proof: a session whose live screen shows the
/// transient-error signature (`API Error: 529 Overloaded`) is detected, nudged
/// to resume, and the round persists its backoff bookkeeping — the lookaside
/// `state` tracks the session (one attempt) and a dynamic `wake_at` is armed for
/// the recheck. The backoff math / escalation / capability branches are covered
/// server-free in `python/weaver-loom/tests/test_resume_program.py`; this proves
/// the screen-scrape → nudge → state+wake plumbing against a live server.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_nudges_a_stalled_session_and_arms_backoff() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    use crate::fixtures::{connect_terminal, drain_until, send_input};
    use std::time::Duration;

    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "resume me").await;

    // Make the session's live screen show the overload error: echo it into the
    // shell and wait for it to render before we survey.
    let mut term = connect_terminal(&ts.addr, &session_id).await;
    send_input(&mut term, "echo API Error: 529 Overloaded\r").await;
    let screen = drain_until(&mut term, "529 Overloaded", Duration::from_secs(5)).await;
    assert!(
        screen.contains("529 Overloaded"),
        "the overload line rendered on the session screen: {screen:?}"
    );

    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "resume-watch".to_string(),
            trigger_spec: json!({ "on": ["session.idle", "session.stale"] }).to_string(),
            scope: json!({}).to_string(),
            program: "builtin:resume".to_string(),
            capabilities: vec![
                "observe".to_string(),
                "nudge".to_string(),
                "mark".to_string(),
            ],
            ..Default::default()
        },
    )
    .await;

    // A manual round surveys the fleet, detects the stall, and nudges.
    let run_id = watch::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();

    // The run recorded a real `nudge` of the stalled session.
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    let run = runs.iter().find(|r| r.id == run_id).unwrap();
    assert_eq!(run.outcome, "ok", "summary: {}", run.summary);
    let actions: serde_json::Value = serde_json::from_str(&run.actions).unwrap();
    assert!(
        actions
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == "nudge" && a["session"] == session_id.as_str()),
        "the round nudges the stalled session: {actions}"
    );

    // The watch now tracks the session (one attempt) and has armed a wake
    // for the backoff recheck — the lookaside-state + dynamic-wake primitives.
    let after = watch_store::get(&state.db, &o.id).await.unwrap().unwrap();
    let tracked = &after.state()[session_id.as_str()];
    assert_eq!(
        tracked["attempts"],
        1,
        "first attempt tracked: {}",
        after.state()
    );
    assert!(
        after.wake_at.is_some(),
        "a backoff recheck wake is armed: {after:?}"
    );

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// T6: the timer half emits a `cron` system tick for a due scheduled watch
/// (visible in the event log) and advances its `next_run_at`; that tick then
/// drives a round through the dispatcher — the producer→consumer chain unattended.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timer_emits_cron_tick_for_a_due_watch_and_dispatches_it() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "tick me").await;
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();

    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            // `every` sugar so the next-fire is deterministic duration arithmetic.
            trigger_spec: json!({ "every": "30m" }).to_string(),
            name: "hourly".to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: survey_program(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;

    // Force it due: set next_run_at into the past.
    watch_store::set_schedule(&state.db, &o.id, None, Some("2000-01-01T00:00:00.000Z"))
        .await
        .unwrap();

    let watermark = events::max_id(&state.db).await.unwrap();
    watch::tick_timer(&state).await;

    // The tick is a first-class, logged `cron` system event carrying our id.
    let new_events = events::since(&state.db, watermark).await.unwrap();
    let cron = new_events
        .iter()
        .find(|e| e.kind == "cron" && e.data["watch"] == o.id.as_str())
        .expect("the timer emits a cron tick for the due watch");
    assert!(
        events::is_system(&cron.branch_id),
        "a cron tick is a fleet-global (system) row"
    );

    // It advanced next_run_at into the future, so it won't re-fire every tick.
    let after = watch_store::get(&state.db, &o.id).await.unwrap().unwrap();
    assert!(
        after.next_run_at.is_some(),
        "the timer advances next_run_at"
    );
    assert_ne!(
        after.next_run_at.as_deref(),
        Some("2000-01-01T00:00:00.000Z"),
        "next_run_at moved forward off the past due time"
    );

    // The dispatcher consumes that cron tick and runs the scheduled round; the
    // run row records the survey of the in-scope session.
    let in_flight = watch::new_in_flight();
    watch::dispatch(&state, &in_flight, cron).await;
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1, "the cron tick fires exactly one round");
    assert_eq!(
        surveyed_ids(&runs[0]),
        vec![session_id.clone()],
        "the scheduled round surveyed the in-scope session"
    );

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// Guardrails: a cooldown re-fire and an in-flight re-fire are both refused — the
/// cooldown one recorded `skipped`, the in-flight one dropped silently (it never
/// opened a run row, because it never started).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cooldown_and_overlap_refire_are_refused() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, branch_id, repo_root) = make_session(&ts, "cool down").await;
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "blocked", "by": "agent" }),
        )
        .await
        .unwrap();

    // A reactive watch with a long cooldown, so a second reactive fire
    // inside the gap is skipped. (A manual run-now bypasses cooldown by design,
    // so the reactive path is what exercises this guardrail.)
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "cooldown-watch".to_string(),
            trigger_spec: json!({ "event": "attention", "repo": repo_root }).to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: survey_program(),
            capabilities: vec!["observe".to_string()],
            cooldown_secs: 3600,
            ..Default::default()
        },
    )
    .await;

    let in_flight = watch::new_in_flight();
    let ev = events::Event {
        id: 0,
        branch_id: branch_id.clone(),
        kind: "tag".to_string(),
        data: json!({ "key": "attention", "value": "blocked" }),
        created_at: db::now_iso(),
    };

    // First reactive fire runs; it stamps `last_run_at`.
    watch::dispatch(&state, &in_flight, &ev).await;
    // Second reactive fire inside the cooldown gap is recorded `skipped`.
    watch::dispatch(&state, &in_flight, &ev).await;

    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 2, "both fires produce a run row");
    assert_eq!(runs[0].outcome, "skipped", "the second fire is on cooldown");
    assert!(
        runs[0].summary.contains("cooldown"),
        "the skip says why: {}",
        runs[0].summary
    );
    assert_eq!(runs[1].outcome, "ok", "the first fire ran");

    // No-overlap: with the watch's id already in the in-flight set, a fire
    // is dropped (no new run row) — a round of it is conceptually already
    // running.
    {
        let set = watch::new_in_flight();
        set.lock().await.insert(o.id.clone());
        let before = watch_store::recent_runs(&state.db, &o.id, 100)
            .await
            .unwrap()
            .len();
        let dropped = watch::fire(
            &state,
            &set,
            &o,
            "event:attention",
            false,
            &watch::TriggerCtx::reactive("session.attention"),
        )
        .await;
        assert!(dropped.is_none(), "an in-flight re-fire is dropped");
        let after = watch_store::recent_runs(&state.db, &o.id, 100)
            .await
            .unwrap()
            .len();
        assert_eq!(before, after, "a dropped re-fire opens no run row");
    }

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// T8/T9: the operator REST surface end to end — create via POST, read back via
/// GET, enable via PATCH, fire a `dry_run` round via POST /run (the audit row
/// comes back with an outcome), list the round history via GET /runs, then
/// DELETE. Plus the validation gates: a bad capability and a duplicate name are
/// both rejected.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_watch_lifecycle_and_validation() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    // A session in scope plus a stubbed judge that recommends a tag, so the
    // dry-run round has something to "would-tag".
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "rest me").await;
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();
    let _agent = fake_judge_agent(
        "weaver-fake-judge-lifecycle",
        r#"[{"key":"review","value":"attention","note":"looks done"}]"#,
    );

    // Create: structured trigger/scope/params JSON in, a WatchView out.
    let created = ts
        .client
        .post(
            "/api/watches",
            json!({
                "name": "rest-watch",
                "trigger": { "cron": "0 * * * *" },
                "scope": { "attention": "!ok" },
                "program": "builtin:status",
                "params": { "prompt": "is it stuck?" },
                "capabilities": ["observe", "mark"],
            }),
        )
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "rest-watch");
    assert_eq!(created["enabled"], false, "new watches start disabled");
    // JSON-bearing fields come back parsed, not as strings.
    assert_eq!(created["trigger"]["cron"], "0 * * * *");
    assert_eq!(created["scope"]["attention"], "!ok");
    assert_eq!(created["params"]["prompt"], "is it stuck?");
    assert_eq!(created["capabilities"][1], "mark");
    assert!(created["last_outcome"].is_null(), "never run yet");

    // A duplicate name is rejected (the client surfaces the error status).
    let dup = ts
        .client
        .post("/api/watches", json!({ "name": "rest-watch" }))
        .await;
    assert!(dup.is_err(), "a duplicate name is rejected");

    // A bad capability is rejected at create time.
    let bad_cap = ts
        .client
        .post(
            "/api/watches",
            json!({ "name": "bad", "capabilities": ["observe", "teleport"] }),
        )
        .await;
    assert!(bad_cap.is_err(), "an unknown capability is rejected");

    // GET by id (resolve also accepts the name).
    let got = ts.client.get(&format!("/api/watches/{id}")).await.unwrap();
    assert_eq!(got["name"], "rest-watch");
    let by_name = ts.client.get("/api/watches/rest-watch").await.unwrap();
    assert_eq!(by_name["id"], id.as_str());

    // It shows up in the list.
    let list = ts.client.get("/api/watches").await.unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["id"] == id.as_str()));

    // Enable via PATCH, and update a mutable field in the same call.
    let patched = ts
        .client
        .patch(
            &format!("/api/watches/{id}"),
            json!({ "enabled": true, "cooldown_secs": 120 }),
        )
        .await
        .unwrap();
    assert_eq!(patched["enabled"], true);
    assert_eq!(patched["cooldown_secs"], 120, "a mutable field updates");

    // Dry-run a round: it returns a run id + outcome, applies no mark.
    let run = ts
        .client
        .post(
            &format!("/api/watches/{id}/run"),
            json!({ "dry_run": true }),
        )
        .await
        .unwrap();
    let run_id = run["run_id"].as_i64().unwrap();
    assert!(run_id > 0, "a run row was opened");
    assert_eq!(
        run["outcome"], "ok",
        "the round surveyed the scoped session"
    );
    let view = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert!(
        branch_tag(&view, "review").is_none(),
        "a dry run applies no mark"
    );

    // GET /runs returns the audit history with actions parsed back to JSON.
    let runs = ts
        .client
        .get(&format!("/api/watches/{id}/runs?limit=10"))
        .await
        .unwrap();
    let runs = runs.as_array().unwrap();
    assert!(!runs.is_empty(), "the round is in the history");
    assert_eq!(runs[0]["id"], run_id);
    assert!(
        runs[0]["actions"].is_array(),
        "actions come back as parsed JSON"
    );

    // DELETE.
    let deleted = ts
        .client
        .delete(&format!("/api/watches/{id}"))
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    let gone = ts.client.get(&format!("/api/watches/{id}")).await;
    assert!(gone.is_err(), "a deleted watch 404s");

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Builtin script programs
// ---------------------------------------------------------------------------

use loom::builtins::python3_available;

/// A stored PR snapshot for a branch, as the GitHub poll loop would write it.
fn pr_snapshot(state: &str, number: i64) -> weaver_core::github::GithubStatus {
    weaver_core::github::GithubStatus {
        pr_number: number,
        pr_url: format!("https://example/pr/{number}"),
        pr_state: state.to_string(),
        pr_title: "the change".to_string(),
        is_draft: false,
        review_decision: None,
        checks: Some("passing".to_string()),
        mergeable: None,
        merged_at: (state == "MERGED").then(db::now_iso),
        fetched_at: db::now_iso(),
    }
}

/// The registry over REST: every builtin carries its defaults, script programs
/// carry their read-only source, and `validate_program` rejects an unknown
/// builtin at create time (naming the registry) while accepting a known one.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_lists_builtin_programs_and_validates_program_refs() {
    let ts = TestServer::start().await;

    let programs = ts.client.get("/api/watches/programs").await.unwrap();
    let arr = programs.as_array().unwrap();
    let names: Vec<&str> = arr.iter().map(|p| p["program"].as_str().unwrap()).collect();
    for expected in [
        "builtin:status",
        "builtin:resume",
        "builtin:pr-label",
        "builtin:review-wait",
        "builtin:archive-merged",
    ] {
        assert!(names.contains(&expected), "{expected} missing: {names:?}");
    }

    // review-wait wakes on the review-decision edge and opts into `mark` so it
    // can park / un-park the row (the others here are read-only).
    let review = arr
        .iter()
        .find(|p| p["program"] == "builtin:review-wait")
        .unwrap();
    assert_eq!(review["defaults"]["trigger"]["on"][0], "pr.review_changed");
    assert!(
        review["defaults"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "mark"),
        "review-wait requests the mark capability to park the row"
    );

    // The resume builtin wakes on the quiet signals and opts into `nudge`.
    let resume = arr
        .iter()
        .find(|p| p["program"] == "builtin:resume")
        .unwrap();
    assert_eq!(resume["defaults"]["trigger"]["on"][0], "session.idle");
    assert!(
        resume["defaults"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "nudge"),
        "resume requests the nudge capability to re-prompt"
    );

    // Every builtin is a script: it ships its source and suggested defaults.
    let archive = arr
        .iter()
        .find(|p| p["program"] == "builtin:archive-merged")
        .unwrap();
    assert!(
        archive["source"]
            .as_str()
            .unwrap()
            .contains("archive-merged"),
        "the embedded source is served"
    );
    // archive-merged subscribes to the PR-merged event, not a polling timer.
    assert_eq!(archive["defaults"]["trigger"]["on"][0], "pr.merged");
    assert_eq!(archive["defaults"]["capabilities"][0], "observe");
    let status = arr
        .iter()
        .find(|p| p["program"] == "builtin:status")
        .unwrap();
    assert!(
        status["source"].as_str().unwrap().contains("session.idle"),
        "the status program is a script like every other builtin"
    );
    // The status builtin wakes on the agent's finished-turn hook, not a timer.
    assert_eq!(status["defaults"]["trigger"]["on"][0], "session.idle");

    // An unknown builtin is rejected at create time; a known script accepted.
    let bad = ts
        .client
        .post(
            "/api/watches",
            json!({ "name": "bad", "program": "builtin:nope" }),
        )
        .await;
    assert!(bad.is_err(), "an unknown builtin program is rejected");
    let ok = ts
        .client
        .post(
            "/api/watches",
            json!({ "name": "good", "program": "builtin:archive-merged" }),
        )
        .await
        .unwrap();
    assert_eq!(ok["program"], "builtin:archive-merged");
}

/// The embedded builtin scripts end to end: each runs as a real `python3`
/// subprocess against the live test server's REST API. `archive-merged` flags
/// the session whose stored PR snapshot is merged; `pr-label` flags the one
/// with an open PR (the fixture repo has no GitHub remote, so the label read
/// degrades to the ensure-label report). Both are read-only — the fleet is
/// untouched afterward.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_scripts_report_merged_and_unlabelled_prs() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (merged_id, merged_branch, _repo) = make_session(&ts, "merged work").await;
    let (open_id, open_branch, _repo) = make_session(&ts, "open work").await;
    loom::github::upsert_status(&state.db, &merged_branch, &pr_snapshot("MERGED", 41))
        .await
        .unwrap();
    loom::github::upsert_status(&state.db, &open_branch, &pr_snapshot("OPEN", 42))
        .await
        .unwrap();

    // archive-merged: exactly the merged session is reported, as a would-do.
    enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "archive-watch".to_string(),
            program: "builtin:archive-merged".to_string(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;
    let run_id = watch::fire_now(&state, "archive-watch", false, "manual")
        .await
        .unwrap();
    let runs = ts
        .client
        .get("/api/watches/archive-watch/runs?limit=10")
        .await
        .unwrap();
    let run = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == run_id)
        .unwrap()
        .clone();
    assert_eq!(run["outcome"], "ok", "summary: {}", run["summary"]);
    let actions = run["actions"].as_array().unwrap();
    assert_eq!(
        actions.len(),
        1,
        "only the merged PR is flagged: {actions:?}"
    );
    assert_eq!(actions[0]["would"], "archive");
    assert_eq!(actions[0]["session"], merged_id.as_str());
    assert_eq!(actions[0]["pr"], 41);

    // pr-label: exactly the open PR is reported, with the default label.
    enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "label-watch".to_string(),
            program: "builtin:pr-label".to_string(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;
    let run_id = watch::fire_now(&state, "label-watch", false, "manual")
        .await
        .unwrap();
    let runs = ts
        .client
        .get("/api/watches/label-watch/runs?limit=10")
        .await
        .unwrap();
    let run = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == run_id)
        .unwrap()
        .clone();
    assert_eq!(run["outcome"], "ok", "summary: {}", run["summary"]);
    let actions = run["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "only the open PR is flagged: {actions:?}");
    assert_eq!(actions[0]["would"], "label");
    assert_eq!(actions[0]["session"], open_id.as_str());
    assert_eq!(actions[0]["label"], "weaver");

    // Read-only: neither session was archived or otherwise mutated.
    for id in [&merged_id, &open_id] {
        let view = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
        assert_ne!(view["status"], "archived", "builtin scripts mutate nothing");
    }

    for id in [merged_id, open_id] {
        ts.client
            .delete(&format!("/api/sessions/{id}"))
            .await
            .unwrap();
    }
}

/// review-wait end to end: the embedded script runs as a real `python3`
/// subprocess against the live server and *mutates* — it parks a session whose
/// PR awaits an external review (`REVIEW_REQUIRED`) with the quiet
/// `awaiting: review` mark, and un-parks it once the review lands. Proves the
/// wiring (script → REST → tag, attributed) and the park/un-park reconcile that
/// pytest covers in isolation.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_wait_parks_and_unparks_a_session_awaiting_review() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (sid, branch, _repo) = make_session(&ts, "review me").await;

    // A PR open and awaiting an external review.
    let mut snap = pr_snapshot("OPEN", 7);
    snap.review_decision = Some("REVIEW_REQUIRED".to_string());
    loom::github::upsert_status(&state.db, &branch, &snap)
        .await
        .unwrap();

    enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "review-wait".to_string(),
            program: "builtin:review-wait".to_string(),
            capabilities: vec!["observe".to_string(), "mark".to_string()],
            ..Default::default()
        },
    )
    .await;

    // Park: the round stamps `awaiting: review`, attributed to the watch.
    watch::fire_now(&state, "review-wait", false, "manual")
        .await
        .unwrap();
    let view = ts
        .client
        .get(&format!("/api/sessions/{sid}"))
        .await
        .unwrap();
    assert_eq!(
        branch_tag_value(&view, "awaiting"),
        "review",
        "the session awaiting review is parked"
    );
    assert_eq!(
        branch_tag(&view, "awaiting").unwrap()["set_by"],
        "review-wait",
        "the parked mark carries the watch's attribution"
    );

    // Review lands → un-park: the next round clears the watch's own mark.
    snap.review_decision = Some("APPROVED".to_string());
    loom::github::upsert_status(&state.db, &branch, &snap)
        .await
        .unwrap();
    watch::fire_now(&state, "review-wait", false, "manual")
        .await
        .unwrap();
    let view = ts
        .client
        .get(&format!("/api/sessions/{sid}"))
        .await
        .unwrap();
    assert!(
        branch_tag(&view, "awaiting").is_none(),
        "the session is un-parked once the review lands"
    );

    ts.client
        .delete(&format!("/api/sessions/{sid}"))
        .await
        .unwrap();
}

/// The LLM judgement path end to end: the status script calls the daemon's
/// `POST /api/agent/oneshot` for its verdict and applies the parsed tag set.
/// First drives the endpoint directly (a stubbed `cat` echoes the prompt; an
/// empty prompt 400s), then runs a round whose stubbed judge recommends a
/// `blocked` mark on an otherwise-calm session — the tag lands attributed,
/// proving the verdict (not a mirror) drove it.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_judgement_uses_the_oneshot_agent() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;

    // The endpoint itself: `cat` echoes the prompt; an empty prompt 400s.
    std::env::set_var("WEAVER_WATCH_AGENT_CMD", "cat");
    let reply = ts
        .client
        .post("/api/agent/oneshot", json!({ "prompt": "ping" }))
        .await
        .unwrap();
    assert_eq!(reply["output"], "ping", "the stub agent echoes the prompt");
    assert!(
        ts.client
            .post("/api/agent/oneshot", json!({ "prompt": "" }))
            .await
            .is_err(),
        "an empty prompt is rejected"
    );

    // A calm session (no attention tag): a `blocked` verdict proves the judge
    // drove the mark — there is no fallback that would have invented one.
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "judge me").await;
    let _agent = fake_judge_agent(
        "weaver-fake-judge-oneshot",
        r#"[{"key":"stuck","value":"blocked","note":"the judge says so"}]"#,
    );
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "judge-watch".to_string(),
            trigger_spec: json!({ "on": ["session.idle"] }).to_string(),
            program: "builtin:status".to_string(),
            params: json!({ "prompt": "is it stuck?" }).to_string(),
            capabilities: vec!["observe".to_string(), "mark".to_string()],
            ..Default::default()
        },
    )
    .await;
    watch::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();

    let view = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(
        branch_tag_value(&view, "stuck"),
        "blocked",
        "the judge's verdict lands as a typed mark"
    );
    // (Parsing detail — the JSON tag-set extraction — is pytest-covered in
    // weaver_loom; here only the through-the-stack outcome matters.)
    assert_eq!(branch_tag(&view, "stuck").unwrap()["set_by"], "judge-watch");

    // Restore the fixture's no-op agent for the tests that follow.
    std::env::set_var("WEAVER_WATCH_AGENT_CMD", "true");
    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// T12 — Warm-session lifecycle
// ---------------------------------------------------------------------------

/// Set a config key on the engine db (the registry-validated path).
async fn set_config(state: &AppState, key: &str, value: &str) {
    core_config::apply(&state.db, &[(key.to_string(), Some(value.to_string()))])
        .await
        .unwrap();
}

/// Insert a managed (warm) session row directly, owned by `watch_id`. The
/// branch is a throwaway in the test repo. Returns the session id. A direct
/// insert keeps the hide/reconcile logic deterministic without standing up a
/// real agent.
async fn insert_managed_session(
    state: &AppState,
    repo_root: &str,
    watch_id: &str,
    term_session: &str,
    work_dir: &str,
) -> String {
    let branch = weaver_core::branch::upsert(&state.db, repo_root, "weaver/watch-warm", "main")
        .await
        .unwrap();
    let id = weaver_core::branch::new_id();
    session_mod::insert(
        &state.db,
        &session_mod::NewSession {
            id: id.clone(),
            branch_id: branch.id,
            work_dir: work_dir.to_string(),
            term_session: term_session.to_string(),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: Some(watch_id.to_string()),
            created_by: None,
        },
    )
    .await
    .unwrap();
    id
}

/// T12: a managed (warm) session is hidden from the fleet — it appears in neither
/// the dashboard `/sessions` listing nor a watch round's survey — while an
/// ordinary session does.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_session_is_hidden_from_fleet_and_survey() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;

    // An ordinary fleet session, reporting non-ok so the survey would mark it.
    let (visible_id, _branch_id, repo_root) = make_session(&ts, "visible work").await;
    ts.client
        .put(
            &format!("/api/sessions/{visible_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();

    // A watch plus its warm session (inserted directly, also non-ok).
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "warm-watch".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: survey_program(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;
    let warm_id = insert_managed_session(
        &state,
        &repo_root,
        &o.id,
        "weaver-warm-hidden",
        "/tmp/warm-hidden",
    )
    .await;
    // The warm session reports non-ok too, so the only thing keeping it out of
    // the survey is the visibility filter, not the scope predicate.
    let warm = session_mod::get(&state.db, &warm_id)
        .await
        .unwrap()
        .unwrap();
    weaver_core::tags::set(
        &state.db,
        &warm.branch_id,
        weaver_core::tags::ATTENTION_KEY,
        "blocked",
        "",
        "agent",
    )
    .await
    .unwrap();

    // The dashboard listing shows the ordinary session, not the warm one.
    let list = ts.client.get("/api/sessions").await.unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&visible_id.as_str()),
        "fleet shows ordinary work"
    );
    assert!(
        !ids.contains(&warm_id.as_str()),
        "fleet hides the warm session: {ids:?}"
    );

    // A round surveys the ordinary session but never the warm one — asserted
    // on the run row the engine records, not on any program side-effect.
    let run_id = watch::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();
    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    let run = runs.iter().find(|r| r.id == run_id).unwrap();
    let surveyed = surveyed_ids(run);
    assert!(
        surveyed.contains(&visible_id),
        "the round surveyed the ordinary session: {surveyed:?}"
    );
    assert!(
        !surveyed.contains(&warm_id),
        "the round never surveyed the warm session: {surveyed:?}"
    );

    ts.client
        .delete(&format!("/api/sessions/{visible_id}"))
        .await
        .unwrap();
}

/// T12: a warm session survives a daemon restart independent of
/// `server.auto_adopt`. With auto-adopt OFF and `watch.adopt_warm` ON, the
/// managed reconcile pass re-adopts a warm session whose terminal is gone — and the
/// inverse: a warm session whose owning watch was deleted is archived, not
/// adopted.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_session_is_re_adopted_across_restart_independent_of_auto_adopt() {
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;

    // The restart policy under test: fleet auto-adopt off, warm-adopt on.
    set_config(&state, "server.auto_adopt", "false").await;
    set_config(&state, "watch.adopt_warm", "true").await;
    // Warm sessions launch the default agent; pin it to `shell` so creation is
    // deterministic without a real `claude` on PATH.
    set_config(&state, "agent.default", "shell").await;

    let repo_root = ts.repo_path().canonicalize().unwrap().display().to_string();

    // A warm watch, scoped to the test repo so its warm session anchors here.
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "memory-watch".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            scope: json!({ "repo": repo_root }).to_string(),
            params: json!({ "warm": true }).to_string(),
            program: survey_program(),
            ..Default::default()
        },
    )
    .await;

    // First need: the engine creates the warm session (a real shell terminal).
    let warm_id = watch::ensure_warm_session(&state, &o)
        .await
        .unwrap()
        .expect("a warm watch gets a session");
    let warm = session_mod::get(&state.db, &warm_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        backend::has_session(&warm.term_session).await,
        "the warm session has a live terminal"
    );
    assert_eq!(
        watch_store::get(&state.db, &o.id)
            .await
            .unwrap()
            .unwrap()
            .warm_session_id
            .as_deref(),
        Some(warm_id.as_str()),
        "the watch is linked to its warm session"
    );

    // Simulate the daemon being down: its terminal is gone, the row remains.
    backend::kill_session(&warm.term_session).await.ok();
    assert!(
        !backend::has_session(&warm.term_session).await,
        "terminal is gone, as after a restart"
    );

    // The managed reconcile pass — the startup adopt for warm sessions — runs even
    // though `server.auto_adopt` is false, and recreates the terminal.
    server::reconcile_managed_sessions(&state).await;
    // Adoption recreates the SAME row's terminal; poll briefly for the async launch.
    let mut recreated = false;
    for _ in 0..40 {
        if backend::has_session(&warm.term_session).await {
            recreated = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        recreated,
        "the warm session's terminal is recreated by warm-adopt"
    );

    // The session id and the watch linkage are stable across the restart.
    let still = session_mod::get(&state.db, &warm_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.id, warm_id, "the warm session id is stable");
    assert_eq!(
        still.managed_by.as_deref(),
        Some(o.id.as_str()),
        "it is still owned by its watch"
    );
    assert_eq!(
        watch_store::get(&state.db, &o.id)
            .await
            .unwrap()
            .unwrap()
            .warm_session_id
            .as_deref(),
        Some(warm_id.as_str()),
        "the warm_session_id linkage survives the restart"
    );

    // Inverse: a warm session whose owner is gone is archived, not adopted.
    backend::kill_session(&still.term_session).await.ok();
    watch_store::delete(&state.db, &o.id).await.unwrap();
    server::reconcile_managed_sessions(&state).await;
    let orphaned = session_mod::get(&state.db, &warm_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        orphaned.status, "archived",
        "an owner-less warm session is archived"
    );
    assert!(
        !backend::has_session(&orphaned.term_session).await,
        "an archived warm session has no terminal (not re-adopted)"
    );
}

/// T12: the engine reuses one warm session across rounds — asked twice to ensure
/// a warm session for the same watch, it returns the same id and spawns no
/// duplicate (the reuse that gives across-round memory).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_warm_session_reuses_the_same_session() {
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    set_config(&state, "agent.default", "shell").await;

    let repo_root = ts.repo_path().canonicalize().unwrap().display().to_string();
    let o = enabled_watch(
        &state,
        watch_store::NewWatch {
            name: "reuse-watch".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            scope: json!({ "repo": repo_root }).to_string(),
            params: json!({ "warm": true }).to_string(),
            program: survey_program(),
            ..Default::default()
        },
    )
    .await;

    let first = watch::ensure_warm_session(&state, &o)
        .await
        .unwrap()
        .unwrap();
    // Re-fetch so the second call sees the persisted `warm_session_id` linkage.
    let o = watch_store::get(&state.db, &o.id).await.unwrap().unwrap();
    let second = watch::ensure_warm_session(&state, &o)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, second, "the same warm session id is reused");

    // Exactly one managed session exists for this watch — no duplicate spawn.
    let managed = session_mod::list_managed(&state.db).await.unwrap();
    let owned: Vec<_> = managed
        .iter()
        .filter(|s| s.managed_by.as_deref() == Some(o.id.as_str()))
        .collect();
    assert_eq!(owned.len(), 1, "no duplicate warm session is spawned");

    // Clean up the warm session's terminal (the harness kills the whole socket too).
    if let Some(s) = session_mod::get(&state.db, &first).await.unwrap() {
        backend::kill_session(&s.term_session).await.ok();
    }
}

/// The end-to-end subscription path: a watch created over REST has its trigger
/// reconciled from the script's register-mode manifest; a `pr_merged` event
/// normalizes to `pr.merged`, wakes the subscribed watch, and the round —
/// handed the triggering session — surveys only that branch (not the whole
/// fleet). The run row records the captured execution log (stdout, exit code,
/// trigger event), which is the watch execution log the UI renders.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_merged_event_scopes_round_to_one_session_and_logs_output() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (merged_id, merged_branch, _repo) = make_session(&ts, "merged work").await;
    // A second live session in the same repo: it must NOT be surveyed, proving
    // the round scoped to the triggering branch instead of the whole fleet.
    let (_other_id, _other_branch, _repo) = make_session(&ts, "other work").await;

    // Create the watch over REST: the script declares `on: [pr.merged]`, and the
    // register-mode reconcile stores it (the script — not the caller — picks the
    // event), with no explicit trigger in the request.
    let created = ts
        .client
        .post(
            "/api/watches",
            json!({
                "name": "merge-watch",
                "program": survey_triggered_program(),
                "capabilities": ["observe"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        created["trigger"]["on"][0], "pr.merged",
        "the stored trigger came from the script's manifest: {created:?}"
    );
    ts.client
        .patch("/api/watches/merge-watch", json!({ "enabled": true }))
        .await
        .unwrap();
    let o = watch_store::get_by_name(&state.db, "merge-watch")
        .await
        .unwrap()
        .unwrap();

    // A merged-PR edge on the merged branch normalizes to `pr.merged`.
    let ev = events::Event {
        id: 0,
        branch_id: merged_branch.clone(),
        kind: "pr_merged".to_string(),
        data: json!({ "pr": 41 }),
        created_at: db::now_iso(),
    };
    let in_flight = watch::new_in_flight();
    watch::dispatch(&state, &in_flight, &ev).await;

    let runs = watch_store::recent_runs(&state.db, &o.id, 10)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1, "the pr.merged event fired exactly one round");
    let run = &runs[0];
    assert_eq!(run.outcome, "ok", "summary: {}", run.summary);
    assert_eq!(run.trigger_event, "pr.merged", "the run records the event");
    assert_eq!(
        surveyed_ids(run),
        vec![merged_id.clone()],
        "the round surveyed only the triggering session, not the fleet"
    );
    // The execution log captured the script's output and a clean exit.
    assert!(
        run.stdout.contains("surveyed 1"),
        "stdout captured: {}",
        run.stdout
    );
    assert_eq!(run.exit_code, Some(0));
    assert!(run.duration_ms.is_some());

    ts.client
        .delete(&format!("/api/sessions/{merged_id}"))
        .await
        .unwrap();
}
