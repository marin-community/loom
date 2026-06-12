//! The Overlooker engine's **wiring**: the dispatcher's event→trigger
//! matching, rounds landing marks and audit rows against the live server, and
//! the guardrails (cooldown / no-overlap → `skipped`).
//!
//! Test placement: a builtin program's *decision logic* is covered
//! server-free by pytest (`python/weaver-loom/tests/`); these cases prove the
//! plumbing — the script runs under the engine, reaches the fleet over REST,
//! and its mutations land with attribution. Don't re-test program logic here.
//!
//! These cases drive the engine **directly** on the test server's isolated db
//! rather than through the spawned background loop: the `overlooker.enabled`
//! master switch is left at its default (off), so the daemon's own engine idles
//! and never races these deterministic calls. Each test builds its own
//! `AppState` over the same isolated db (the harness exports `WEAVER_HOME`) and
//! calls the public engine seams — `dispatch`, `fire_now`, `new_in_flight` /
//! `fire` — so a round runs without waiting on the timer.

use std::collections::HashSet;

use chrono::Utc;
use serde_json::json;
use serial_test::serial;

use loom::events::EventBus;
use loom::web::AppState;
use loom::{db, events, monitor, overlooker, server, session as session_mod, tmux};
use weaver_core::config as core_config;
use weaver_core::overlooker as ov;

use crate::fixtures::{branch_tag, branch_tag_value, TestServer};

/// An `AppState` over the test server's isolated db — a second connection to the
/// same sqlite file (WAL, so concurrent readers/writers are fine). Lets a test
/// call the in-process engine seams the spawned loop would otherwise own.
async fn engine_state(ts: &TestServer) -> AppState {
    let pool = db::connect(&db::default_db_path()).await.unwrap();
    AppState {
        db: pool,
        bus: EventBus::new(),
        addr: ts.addr.to_string(),
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

/// Register an enabled overlooker and return it.
async fn enabled_overlooker(state: &AppState, new: ov::NewOverlooker) -> ov::Overlooker {
    let o = ov::create(&state.db, &new).await.unwrap();
    ov::set_enabled(&state.db, &o.id, true).await.unwrap();
    ov::get(&state.db, &o.id).await.unwrap().unwrap()
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

/// The session ids a run's recorded `survey` actions name.
fn surveyed_ids(run: &ov::OverlookerRun) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(&run.actions)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|a| a["action"] == "survey")
        .filter_map(|a| a["session"].as_str().map(str::to_string))
        .collect()
}

/// T4: a reactive event whose trigger matches fires the right overlooker exactly
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

    // An overlooker reacting to `attention` events in *this* repo, surveying
    // the scoped (non-ok) fleet via the test-owned fixture program.
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "blocked-watch".to_string(),
            trigger_spec: json!({ "event": "attention", "repo": repo_root }).to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: survey_program(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;

    let in_flight = overlooker::new_in_flight();

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
    overlooker::dispatch(&state, &in_flight, &ev).await;

    // Exactly one run, and its run row records the survey of the in-scope
    // session — the engine-observable proof the round ran.
    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
    assert_eq!(runs.len(), 1, "one matching event fires exactly one round");
    assert_eq!(runs[0].outcome, "ok");
    assert_eq!(
        surveyed_ids(&runs[0]),
        vec![session_id.clone()],
        "the round surveyed exactly the in-scope session"
    );

    // Re-firing the identical event is idempotent: level-triggered, the round
    // just re-surveys the current fleet — a fresh run row, same survey.
    overlooker::dispatch(&state, &in_flight, &ev).await;
    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
    assert_eq!(runs.len(), 2, "a re-fire is a fresh round");
    assert_eq!(
        surveyed_ids(&runs[0]),
        vec![session_id.clone()],
        "the re-fired round re-surveys, it does not 'handle' the event again"
    );

    // The repo filter excludes an event from another repo: same kind/level but a
    // branch that lives elsewhere must not fire this overlooker.
    let runs_before = ov::recent_runs(&state.db, &o.id, 100).await.unwrap().len();
    let other_repo_event = events::Event {
        id: 0,
        // A system (branchless) event carries no repo, so the repo-filtered
        // trigger must not match it.
        branch_id: events::SYSTEM_BRANCH.to_string(),
        kind: "tag".to_string(),
        data: json!({ "key": "attention", "value": "blocked" }),
        created_at: db::now_iso(),
    };
    overlooker::dispatch(&state, &in_flight, &other_repo_event).await;
    let runs_after = ov::recent_runs(&state.db, &o.id, 100).await.unwrap().len();
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
/// a one-shot `stale` event, an overlooker with an `{"event":"stale"}` trigger
/// fires a round off it, and the emission is edge-detected — driving the
/// staleness check twice produces exactly one `stale` row, not one per pass.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_session_emits_one_event_and_wakes_a_reactive_overlooker() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "go stale").await;

    // The session reports `attention` about itself so it falls inside the
    // overlooker's `!ok` scope (the survey will name it, proving it ran).
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();

    // A reactive overlooker matching `stale` events fleet-wide.
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
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
    let in_flight = overlooker::new_in_flight();
    overlooker::dispatch(&state, &in_flight, stale_ev).await;
    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
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

/// The status program's wiring proof: a round over a non-ok session lands an
/// attributed triage mark on the live server and records the action on the
/// run row. The program's decision logic (judge fallback, dry-run, capability
/// branches, summaries) is covered server-free in
/// `python/weaver-loom/tests/test_status_program.py`; dry-run flag
/// propagation through the executor is covered by the lib test
/// `run_script_round_trips_the_contract`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_status_round_lands_an_attributed_mark() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "status me").await;

    // The session reports `attention` about itself → it's in a `!ok` scope.
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();

    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "status-check".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            scope: json!({ "attention": "!ok" }).to_string(),
            program: "builtin:status".to_string(),
            capabilities: vec!["observe".to_string(), "mark".to_string()],
            ..Default::default()
        },
    )
    .await;

    // A manual round (run-now) fires the stock program directly.
    let run_id = overlooker::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();

    // The mark landed on the session's branch.
    let view = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(
        branch_tag_value(&view, "triage"),
        "attention",
        "the round marks the in-scope session"
    );
    assert_eq!(
        branch_tag(&view, "triage").unwrap()["set_by"],
        "status-check"
    );

    // The run row records the mark action.
    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
    let run = runs.iter().find(|r| r.id == run_id).unwrap();
    assert_eq!(run.outcome, "ok");
    let actions: serde_json::Value = serde_json::from_str(&run.actions).unwrap();
    let arr = actions.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|a| a["action"] == "mark" && a["session"] == session_id.as_str()),
        "the run records a mark action: {actions}"
    );

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}

/// T6: the timer half emits a `cron` system tick for a due scheduled overlooker
/// (visible in the event log) and advances its `next_run_at`; that tick then
/// drives a round through the dispatcher — the producer→consumer chain unattended.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timer_emits_cron_tick_for_a_due_overlooker_and_dispatches_it() {
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

    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
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
    ov::set_schedule(&state.db, &o.id, None, Some("2000-01-01T00:00:00.000Z"))
        .await
        .unwrap();

    let watermark = events::max_id(&state.db).await.unwrap();
    overlooker::tick_timer(&state).await;

    // The tick is a first-class, logged `cron` system event carrying our id.
    let new_events = events::since(&state.db, watermark).await.unwrap();
    let cron = new_events
        .iter()
        .find(|e| e.kind == "cron" && e.data["overlooker"] == o.id.as_str())
        .expect("the timer emits a cron tick for the due overlooker");
    assert!(
        events::is_system(&cron.branch_id),
        "a cron tick is a fleet-global (system) row"
    );

    // It advanced next_run_at into the future, so it won't re-fire every tick.
    let after = ov::get(&state.db, &o.id).await.unwrap().unwrap();
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
    let in_flight = overlooker::new_in_flight();
    overlooker::dispatch(&state, &in_flight, cron).await;
    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
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

    // A reactive overlooker with a long cooldown, so a second reactive fire
    // inside the gap is skipped. (A manual run-now bypasses cooldown by design,
    // so the reactive path is what exercises this guardrail.)
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
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

    let in_flight = overlooker::new_in_flight();
    let ev = events::Event {
        id: 0,
        branch_id: branch_id.clone(),
        kind: "tag".to_string(),
        data: json!({ "key": "attention", "value": "blocked" }),
        created_at: db::now_iso(),
    };

    // First reactive fire runs; it stamps `last_run_at`.
    overlooker::dispatch(&state, &in_flight, &ev).await;
    // Second reactive fire inside the cooldown gap is recorded `skipped`.
    overlooker::dispatch(&state, &in_flight, &ev).await;

    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
    assert_eq!(runs.len(), 2, "both fires produce a run row");
    assert_eq!(runs[0].outcome, "skipped", "the second fire is on cooldown");
    assert!(
        runs[0].summary.contains("cooldown"),
        "the skip says why: {}",
        runs[0].summary
    );
    assert_eq!(runs[1].outcome, "ok", "the first fire ran");

    // No-overlap: with the overlooker's id already in the in-flight set, a fire
    // is dropped (no new run row) — a round of it is conceptually already
    // running.
    {
        let set = overlooker::new_in_flight();
        set.lock().await.insert(o.id.clone());
        let before = ov::recent_runs(&state.db, &o.id, 100).await.unwrap().len();
        let dropped = overlooker::fire(&state, &set, &o, "event:attention", false).await;
        assert!(dropped.is_none(), "an in-flight re-fire is dropped");
        let after = ov::recent_runs(&state.db, &o.id, 100).await.unwrap().len();
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
async fn rest_overlooker_lifecycle_and_validation() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    // A non-ok session so the dry-run round has something in scope to "would-mark".
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "rest me").await;
    ts.client
        .put(
            &format!("/api/sessions/{session_id}/tags/attention"),
            json!({ "value": "attention", "by": "agent" }),
        )
        .await
        .unwrap();

    // Create: structured trigger/scope/params JSON in, an OverlookerView out.
    let created = ts
        .client
        .post(
            "/api/overlookers",
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
    assert_eq!(created["enabled"], false, "new overlookers start disabled");
    // JSON-bearing fields come back parsed, not as strings.
    assert_eq!(created["trigger"]["cron"], "0 * * * *");
    assert_eq!(created["scope"]["attention"], "!ok");
    assert_eq!(created["params"]["prompt"], "is it stuck?");
    assert_eq!(created["capabilities"][1], "mark");
    assert!(created["last_outcome"].is_null(), "never run yet");

    // A duplicate name is rejected (the client surfaces the error status).
    let dup = ts
        .client
        .post("/api/overlookers", json!({ "name": "rest-watch" }))
        .await;
    assert!(dup.is_err(), "a duplicate name is rejected");

    // A bad capability is rejected at create time.
    let bad_cap = ts
        .client
        .post(
            "/api/overlookers",
            json!({ "name": "bad", "capabilities": ["observe", "teleport"] }),
        )
        .await;
    assert!(bad_cap.is_err(), "an unknown capability is rejected");

    // GET by id (resolve also accepts the name).
    let got = ts
        .client
        .get(&format!("/api/overlookers/{id}"))
        .await
        .unwrap();
    assert_eq!(got["name"], "rest-watch");
    let by_name = ts.client.get("/api/overlookers/rest-watch").await.unwrap();
    assert_eq!(by_name["id"], id.as_str());

    // It shows up in the list.
    let list = ts.client.get("/api/overlookers").await.unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["id"] == id.as_str()));

    // Enable via PATCH, and update a mutable field in the same call.
    let patched = ts
        .client
        .patch(
            &format!("/api/overlookers/{id}"),
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
            &format!("/api/overlookers/{id}/run"),
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
        branch_tag(&view, "triage").is_none(),
        "a dry run applies no mark"
    );

    // GET /runs returns the audit history with actions parsed back to JSON.
    let runs = ts
        .client
        .get(&format!("/api/overlookers/{id}/runs?limit=10"))
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
        .delete(&format!("/api/overlookers/{id}"))
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    let gone = ts.client.get(&format!("/api/overlookers/{id}")).await;
    assert!(gone.is_err(), "a deleted overlooker 404s");

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

    let programs = ts.client.get("/api/overlookers/programs").await.unwrap();
    let arr = programs.as_array().unwrap();
    let names: Vec<&str> = arr.iter().map(|p| p["program"].as_str().unwrap()).collect();
    for expected in [
        "builtin:status",
        "builtin:pr-label",
        "builtin:archive-merged",
    ] {
        assert!(names.contains(&expected), "{expected} missing: {names:?}");
    }

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
    assert!(archive["defaults"]["trigger"]["every"].is_string());
    assert_eq!(archive["defaults"]["capabilities"][0], "observe");
    let status = arr
        .iter()
        .find(|p| p["program"] == "builtin:status")
        .unwrap();
    assert!(
        status["source"].as_str().unwrap().contains("triage"),
        "the status program is a script like every other builtin"
    );

    // An unknown builtin is rejected at create time; a known script accepted.
    let bad = ts
        .client
        .post(
            "/api/overlookers",
            json!({ "name": "bad", "program": "builtin:nope" }),
        )
        .await;
    assert!(bad.is_err(), "an unknown builtin program is rejected");
    let ok = ts
        .client
        .post(
            "/api/overlookers",
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
    enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "archive-watch".to_string(),
            program: "builtin:archive-merged".to_string(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;
    let run_id = overlooker::fire_now(&state, "archive-watch", false, "manual")
        .await
        .unwrap();
    let runs = ts
        .client
        .get("/api/overlookers/archive-watch/runs?limit=10")
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
    enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "label-watch".to_string(),
            program: "builtin:pr-label".to_string(),
            capabilities: vec!["observe".to_string()],
            ..Default::default()
        },
    )
    .await;
    let run_id = overlooker::fire_now(&state, "label-watch", false, "manual")
        .await
        .unwrap();
    let runs = ts
        .client
        .get("/api/overlookers/label-watch/runs?limit=10")
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

/// The LLM judgement path end to end: with a prompt configured and the
/// one-shot agent stubbed (`WEAVER_OVERLOOKER_AGENT_CMD=cat` echoes the
/// prompt back), the status script calls the daemon's `POST /api/agent/oneshot`
/// and parses the level + note out of the reply — the mark carries the
/// agent's judgement, not the attention mirror. Also drives the endpoint
/// directly: a stubbed agent echoes, and an empty prompt is rejected.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_judgement_uses_the_oneshot_agent_when_prompted() {
    if !python3_available() {
        eprintln!("skipping: python3 not on PATH");
        return;
    }
    let ts = TestServer::start().await;
    // `cat` echoes the composed prompt (prompt + screen) straight back, so the
    // judgement parser sees the prompt's own first line.
    std::env::set_var("WEAVER_OVERLOOKER_AGENT_CMD", "cat");

    // The endpoint itself: output echoes the prompt; an empty prompt 400s.
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

    // A calm session (no attention tag): the fallback rule would clear, so a
    // `blocked` mark proves the agent judgement was used.
    let state = engine_state(&ts).await;
    let (session_id, _branch_id, _repo_root) = make_session(&ts, "judge me").await;
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "judge-watch".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            program: "builtin:status".to_string(),
            params: json!({ "prompt": "blocked - the judge says so" }).to_string(),
            capabilities: vec!["observe".to_string(), "mark".to_string()],
            ..Default::default()
        },
    )
    .await;
    overlooker::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();

    let view = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(
        branch_tag_value(&view, "triage"),
        "blocked",
        "the agent judgement (not the calm attention mirror) lands as the mark"
    );
    // (Judgement parsing detail — level/note extraction — is pytest-covered in
    // weaver_loom; here only the through-the-stack outcome matters.)
    assert_eq!(
        branch_tag(&view, "triage").unwrap()["set_by"],
        "judge-watch"
    );

    // Restore the fixture's no-op agent for the tests that follow.
    std::env::set_var("WEAVER_OVERLOOKER_AGENT_CMD", "true");
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

/// Insert a managed (warm) session row directly, owned by `overlooker_id`. The
/// branch is a throwaway in the test repo. Returns the session id. A direct
/// insert keeps the hide/reconcile logic deterministic without standing up a
/// real agent.
async fn insert_managed_session(
    state: &AppState,
    repo_root: &str,
    overlooker_id: &str,
    tmux_session: &str,
    work_dir: &str,
) -> String {
    let branch =
        weaver_core::branch::upsert(&state.db, repo_root, "weaver/overlooker-warm", "main")
            .await
            .unwrap();
    let id = weaver_core::branch::new_id();
    session_mod::insert(
        &state.db,
        &session_mod::NewSession {
            id: id.clone(),
            branch_id: branch.id,
            work_dir: work_dir.to_string(),
            tmux_session: tmux_session.to_string(),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: Some(overlooker_id.to_string()),
        },
    )
    .await
    .unwrap();
    id
}

/// T12: a managed (warm) session is hidden from the fleet — it appears in neither
/// the dashboard `/sessions` listing nor an overlooker round's survey — while an
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

    // An overlooker plus its warm session (inserted directly, also non-ok).
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
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
    let run_id = overlooker::fire_now(&state, &o.name, false, "manual")
        .await
        .unwrap();
    let runs = ov::recent_runs(&state.db, &o.id, 10).await.unwrap();
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
/// `server.auto_adopt`. With auto-adopt OFF and `overlooker.adopt_warm` ON, the
/// managed reconcile pass re-adopts a warm session whose tmux is gone — and the
/// inverse: a warm session whose owning overlooker was deleted is archived, not
/// adopted.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_session_is_re_adopted_across_restart_independent_of_auto_adopt() {
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;

    // The restart policy under test: fleet auto-adopt off, warm-adopt on.
    set_config(&state, "server.auto_adopt", "false").await;
    set_config(&state, "overlooker.adopt_warm", "true").await;
    // Warm sessions launch the default agent; pin it to `shell` so creation is
    // deterministic without a real `claude` on PATH.
    set_config(&state, "agent.default", "shell").await;

    let repo_root = ts.repo_path().canonicalize().unwrap().display().to_string();

    // A warm overlooker, scoped to the test repo so its warm session anchors here.
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "memory-watch".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            scope: json!({ "repo": repo_root }).to_string(),
            params: json!({ "warm": true }).to_string(),
            program: survey_program(),
            ..Default::default()
        },
    )
    .await;

    // First need: the engine creates the warm session (a real shell tmux).
    let warm_id = overlooker::ensure_warm_session(&state, &o)
        .await
        .unwrap()
        .expect("a warm overlooker gets a session");
    let warm = session_mod::get(&state.db, &warm_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tmux::has_session(&warm.tmux_session).await,
        "the warm session has a live tmux"
    );
    assert_eq!(
        ov::get(&state.db, &o.id)
            .await
            .unwrap()
            .unwrap()
            .warm_session_id
            .as_deref(),
        Some(warm_id.as_str()),
        "the overlooker is linked to its warm session"
    );

    // Simulate the daemon being down: its tmux is gone, the row remains.
    tmux::kill_session(&warm.tmux_session).await.ok();
    assert!(
        !tmux::has_session(&warm.tmux_session).await,
        "tmux is gone, as after a restart"
    );

    // The managed reconcile pass — the startup adopt for warm sessions — runs even
    // though `server.auto_adopt` is false, and recreates the tmux.
    server::reconcile_managed_sessions(&state).await;
    // Adoption recreates the SAME row's tmux; poll briefly for the async launch.
    let mut recreated = false;
    for _ in 0..40 {
        if tmux::has_session(&warm.tmux_session).await {
            recreated = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        recreated,
        "the warm session's tmux is recreated by warm-adopt"
    );

    // The session id and the overlooker linkage are stable across the restart.
    let still = session_mod::get(&state.db, &warm_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.id, warm_id, "the warm session id is stable");
    assert_eq!(
        still.managed_by.as_deref(),
        Some(o.id.as_str()),
        "it is still owned by its overlooker"
    );
    assert_eq!(
        ov::get(&state.db, &o.id)
            .await
            .unwrap()
            .unwrap()
            .warm_session_id
            .as_deref(),
        Some(warm_id.as_str()),
        "the warm_session_id linkage survives the restart"
    );

    // Inverse: a warm session whose owner is gone is archived, not adopted.
    tmux::kill_session(&still.tmux_session).await.ok();
    ov::delete(&state.db, &o.id).await.unwrap();
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
        !tmux::has_session(&orphaned.tmux_session).await,
        "an archived warm session has no tmux (not re-adopted)"
    );
}

/// T12: the engine reuses one warm session across rounds — asked twice to ensure
/// a warm session for the same overlooker, it returns the same id and spawns no
/// duplicate (the reuse that gives across-round memory).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_warm_session_reuses_the_same_session() {
    let ts = TestServer::start().await;
    let state = engine_state(&ts).await;
    set_config(&state, "agent.default", "shell").await;

    let repo_root = ts.repo_path().canonicalize().unwrap().display().to_string();
    let o = enabled_overlooker(
        &state,
        ov::NewOverlooker {
            name: "reuse-watch".to_string(),
            trigger_spec: json!({ "cron": "0 * * * *" }).to_string(),
            scope: json!({ "repo": repo_root }).to_string(),
            params: json!({ "warm": true }).to_string(),
            program: survey_program(),
            ..Default::default()
        },
    )
    .await;

    let first = overlooker::ensure_warm_session(&state, &o)
        .await
        .unwrap()
        .unwrap();
    // Re-fetch so the second call sees the persisted `warm_session_id` linkage.
    let o = ov::get(&state.db, &o.id).await.unwrap().unwrap();
    let second = overlooker::ensure_warm_session(&state, &o)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, second, "the same warm session id is reused");

    // Exactly one managed session exists for this overlooker — no duplicate spawn.
    let managed = session_mod::list_managed(&state.db).await.unwrap();
    let owned: Vec<_> = managed
        .iter()
        .filter(|s| s.managed_by.as_deref() == Some(o.id.as_str()))
        .collect();
    assert_eq!(owned.len(), 1, "no duplicate warm session is spawned");

    // Clean up the warm session's tmux (the harness kills the whole socket too).
    if let Some(s) = session_mod::get(&state.db, &first).await.unwrap() {
        tmux::kill_session(&s.tmux_session).await.ok();
    }
}
