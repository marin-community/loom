//! Session ownership and the launch-attempt escape hatches.

use std::time::Duration;

use serde_json::json;
use serial_test::serial;

use loom::backend;
use loom::runs::{self, NewRun, Reservation};

use crate::fixtures::TestServer;

async fn reserve(ts: &TestServer, key: &str) -> runs::Run {
    match runs::reserve(
        &ts.state.db,
        NewRun {
            subject: "automation:test",
            source: "grafana",
            service_tag: "grafana",
            profile: "default",
            idempotency_key: key,
            channel: None,
            request_json: "{}",
        },
    )
    .await
    .unwrap()
    {
        Reservation::Created(run) => run,
        Reservation::Existing(_) => panic!("test reservation must be new"),
    }
}

async fn wait_dead(name: &str) {
    for _ in 0..300 {
        if !backend::has_session(name).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("supervisor {name} did not stop");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_launch_attempt_can_be_archived_then_removed_by_reserved_session_id() {
    let ts = TestServer::start().await;
    let run = reserve(&ts, "failed-attempt").await;
    assert!(runs::failed(&ts.state.db, &run.id, "capacity exhausted")
        .await
        .unwrap());

    let archived = ts
        .client
        .post(
            "/api/sessions/archive",
            json!({ "session": run.session_id }),
        )
        .await
        .unwrap();
    assert_eq!(archived["archived"], true);
    assert_eq!(archived["kind"], "launch_attempt");
    let cancelled = runs::get(&ts.state.db, &run.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(cancelled.summary, "launch attempt archived by user");

    let removed = ts
        .client
        .post("/api/sessions/delete", json!({ "session": run.session_id }))
        .await
        .unwrap();
    assert_eq!(removed["deleted"], true);
    assert_eq!(removed["kind"], "launch_attempt");
    assert!(runs::get(&ts.state.db, &run.id).await.unwrap().is_none());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_creating_attempt_tears_down_its_reserved_runtime() {
    let ts = TestServer::start().await;
    let run = reserve(&ts, "creating-attempt").await;
    let term = format!("weaver-{}", run.session_id);
    backend::new_session(&term, ts.repo_path(), "sleep 60", &[], false, 0)
        .await
        .unwrap();
    assert!(backend::has_session(&term).await);

    ts.client
        .post(
            "/api/sessions/archive",
            json!({ "session": run.session_id }),
        )
        .await
        .unwrap();
    wait_dead(&term).await;
    assert_eq!(
        runs::get(&ts.state.db, &run.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_reconciliation_removes_only_unowned_loom_resources() {
    let ts = TestServer::start().await;
    let active = reserve(&ts, "active-owner").await;
    let owned = format!("weaver-{}", active.session_id);
    let finished = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "finished but inspectable",
                "cwd": ts.cwd(),
                "agent": "shell"
            }),
        )
        .await
        .unwrap();
    let finished_id = finished["id"].as_str().unwrap();
    let finished_term = finished["term_session"].as_str().unwrap();
    ts.client
        .post(
            "/api/sessions/update",
            json!({ "session": finished_id, "status": "done" }),
        )
        .await
        .unwrap();
    let stray = "weaver-no-database-owner";
    let transient = "weaver-acp-prompt-active";
    let stale_transient = "weaver-acp-prompt-stale";
    let unrelated = "other-tapestry-user";
    let transient_lease = ts.state.acp.transient_sessions().lease(transient);
    for name in [&owned, stray, transient, stale_transient, unrelated] {
        backend::new_session(name, ts.repo_path(), "sleep 60", &[], false, 0)
            .await
            .unwrap();
    }

    let report = loom::session_manager::reconcile_supervisors(&ts.state.db, &ts.state.acp)
        .await
        .unwrap();
    assert_eq!(report.removed_agents, 2);
    wait_dead(stray).await;
    wait_dead(stale_transient).await;
    assert!(backend::has_session(&owned).await);
    assert!(
        backend::has_session(transient).await,
        "an in-flight one-shot ACP relay retains process-local ownership"
    );
    assert!(
        backend::has_session(finished_term).await,
        "rollout must preserve supervisors owned by inspectable terminal sessions"
    );
    assert!(backend::has_session(unrelated).await);

    runs::cancel_for_session(&ts.state.db, &active.session_id)
        .await
        .unwrap();
    loom::session_manager::reconcile_supervisors(&ts.state.db, &ts.state.acp)
        .await
        .unwrap();
    wait_dead(&owned).await;
    drop(transient_lease);
    loom::session_manager::reconcile_supervisors(&ts.state.db, &ts.state.acp)
        .await
        .unwrap();
    wait_dead(transient).await;
    ts.client
        .post("/api/sessions/delete", json!({ "session": finished_id }))
        .await
        .unwrap();
    backend::kill_session(unrelated).await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciliation_terminalizes_a_session_that_landed_after_cancellation() {
    let ts = TestServer::start().await;
    let run = reserve(&ts, "cancel-race").await;
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "late automation session",
                "cwd": ts.cwd(),
                "agent": "shell",
                "class": "automation"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let term = session["term_session"].as_str().unwrap();
    sqlx::query("UPDATE sessions SET automation_run_id = ? WHERE id = ?")
        .bind(&run.id)
        .bind(id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE automation_runs SET session_id = ? WHERE id = ?")
        .bind(id)
        .bind(&run.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    runs::cancel_for_session_with_summary(&ts.state.db, id, "launch attempt archived by user")
        .await
        .unwrap();

    let report = loom::session_manager::reconcile_supervisors(&ts.state.db, &ts.state.acp)
        .await
        .unwrap();
    assert_eq!(report.invalidated_sessions, 1);
    wait_dead(term).await;
    let view = ts
        .client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "error");

    ts.client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_inventory_can_see_and_remove_managed_sessions() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "managed inventory",
                "cwd": ts.cwd(),
                "agent": "shell",
                "class": "automation"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    sqlx::query("UPDATE sessions SET managed_by = 'watch-test' WHERE id = ?")
        .bind(id)
        .execute(&ts.state.db)
        .await
        .unwrap();

    // `managed` is the operator escape hatch, off unless asked for: a warm
    // session an engine owns is invisible to an ordinary listing even with
    // automation and history widened all the way open.
    let ordinary = ts
        .client
        .post(
            "/api/sessions/list",
            json!({ "automation": true, "history": true }),
        )
        .await
        .unwrap();
    assert!(ordinary.as_array().unwrap().is_empty());

    let admin = ts
        .client
        .post(
            "/api/sessions/list",
            json!({ "automation": true, "history": true, "managed": true }),
        )
        .await
        .unwrap();
    assert_eq!(admin.as_array().unwrap().len(), 1);
    assert_eq!(admin[0]["id"], id);

    ts.client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_recorded_session_state_can_be_archived_and_removed() {
    let ts = TestServer::start().await;
    for status in ["created", "running", "orphaned", "done", "error"] {
        let session = ts
            .client
            .post(
                "/api/sessions/launch",
                json!({
                    "goal": format!("archive {status}"),
                    "name": format!("archive-{status}"),
                    "cwd": ts.cwd(),
                    "agent": "shell"
                }),
            )
            .await
            .unwrap();
        let id = session["id"].as_str().unwrap();
        ts.client
            .post(
                "/api/sessions/update",
                json!({ "session": id, "status": status }),
            )
            .await
            .unwrap();

        let archived = ts
            .client
            .post("/api/sessions/archive", json!({ "session": id }))
            .await
            .unwrap();
        assert_eq!(archived["archived"], true, "could not archive {status}");
        let view = ts
            .client
            .post("/api/sessions/get", json!({ "session": id }))
            .await
            .unwrap();
        assert_eq!(view["status"], "archived");

        // Archive is idempotent and remove remains available afterwards.
        ts.client
            .post("/api/sessions/archive", json!({ "session": id }))
            .await
            .unwrap();
        ts.client
            .post("/api/sessions/delete", json!({ "session": id }))
            .await
            .unwrap();
    }
}
