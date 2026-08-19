//! Recovering an archived session is the inverse of archiving: it rebuilds the
//! worktree from the kept branch and resumes the agent, flipping the row back out
//! of the terminal `archived` state and into the live fleet — the same shape as
//! adopting an orphaned session, but starting from a torn-down worktree.

use std::path::Path;

use serde_json::json;
use serial_test::serial;

use loom::backend;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_rebuilds_worktree_and_resumes() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "recover me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();
    assert!(
        Path::new(&work_dir).exists(),
        "worktree should exist on launch"
    );

    // Archive tears the worktree down but keeps the branch + row.
    let res = client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(
        !Path::new(&work_dir).exists(),
        "archive should remove the worktree"
    );
    assert!(
        !backend::has_session(&term_session).await,
        "archive should kill the terminal session"
    );
    let view = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(view["status"], "archived");
    let channel = client.get(&format!("/api/channels/{id}")).await.unwrap();
    assert_eq!(channel["state"], "archived");

    // Recover rebuilds the worktree and resumes the agent at the same path.
    let rec = client
        .post(&format!("/api/sessions/{id}/recover"), json!({}))
        .await
        .unwrap();
    // The row is live again (shell is hookless, so it comes up `running`), on the
    // same worktree path and terminal session.
    assert_eq!(rec["status"], "running");
    assert_eq!(rec["work_dir"], work_dir);
    assert_eq!(rec["term_session"], term_session);
    let channel = client.get(&format!("/api/channels/{id}")).await.unwrap();
    assert_eq!(channel["state"], "open");
    let messages = client
        .get(&format!("/api/channels/{id}/messages"))
        .await
        .unwrap();
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["kind"] == "system" && message["body"] == "session recovered"),
        "recovery appends a channel lifecycle message"
    );
    let tags = rec["branch"]["tags"].as_array().unwrap();
    assert!(
        tags.iter()
            .any(|tag| tag["key"] == "recovered" && tag["value"] == "true"),
        "recover should stamp a quiet recovered tag"
    );
    assert!(
        Path::new(&work_dir).exists(),
        "recover should rebuild the worktree on disk"
    );
    assert!(
        backend::has_session(&term_session).await,
        "recover should recreate the terminal session"
    );
    // The kept branch is what got checked back out — recover never re-forks.
    assert!(
        weaver_core::git::branch_exists(ts.repo_path(), "weaver/recover-me").await,
        "recover reuses the archived branch"
    );

    // A recovered session is a normal live session again: it shows in the fleet
    // without the archived opt-in.
    let fleet = client.get("/api/sessions").await.unwrap();
    assert!(
        fleet
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == id.as_str()),
        "recovered session rejoins the default fleet listing"
    );
}

/// Regression for archive/adopt racing on the lifecycle lock: once archive has
/// removed the worktree and committed `archived`, a stale adopt request must
/// direct the caller to recovery instead of reporting that the very worktree
/// archive intentionally removed makes adoption impossible.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_adopt_after_archive_points_to_recovery() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "archive adopt race", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap();

    client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();
    let error = client
        .post(&format!("/api/sessions/{id}/adopt"), json!({}))
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("recover"), "{message}");
    assert!(!message.contains("no longer exists on disk"), "{message}");

    let recovered = client
        .post(&format!("/api/sessions/{id}/recover"), json!({}))
        .await
        .unwrap();
    assert_eq!(recovered["status"], "running");
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transition_stage_is_visible_in_detail_and_fleet_views() {
    let ts = TestServer::start().await;
    let sess = ts
        .client
        .post(
            "/api/sessions",
            json!({ "goal": "show lifecycle progress", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap();
    assert!(
        loom::session::begin_transition(&ts.state.db, id, "archiving", "Removing worktree")
            .await
            .unwrap()
    );

    let detail = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(detail["status"], "running");
    assert_eq!(detail["transition"]["kind"], "archiving");
    assert_eq!(detail["transition"]["step"], "Removing worktree");
    assert!(!detail["transition"]["started_at"]
        .as_str()
        .unwrap()
        .is_empty());

    let fleet = ts.client.get("/api/sessions/summary").await.unwrap();
    let summary = fleet
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(summary["transition"]["kind"], "archiving");
    assert_eq!(summary["transition"]["step"], "Removing worktree");

    loom::session::clear_transition(&ts.state.db, id, "archiving")
        .await
        .unwrap();
    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}

/// Repair an old partial archive whose row says `archived` even though its
/// terminal supervisor survived. New archives wait for teardown before flipping
/// the row, but recovery must self-heal rows written by older loom versions.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_repairs_an_archived_row_with_a_live_terminal() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "still running", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();

    // Recreate the historical bad state directly: the row was flipped even
    // though teardown failed and the supervisor remained live.
    loom::session::set_status(&ts.state.db, &id, "archived")
        .await
        .unwrap();
    assert!(backend::has_session(&term_session).await);

    let recovered = client
        .post(&format!("/api/sessions/{id}/recover"), json!({}))
        .await
        .unwrap();
    assert_eq!(recovered["status"], "running");
    assert!(
        backend::has_session(&term_session).await,
        "repair keeps the already-live agent"
    );
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_recovery_rolls_back_to_a_fully_archived_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "cannot recover", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let branch = sess["branch"]["branch"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();

    client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();
    weaver_core::git::delete_branch(ts.repo_path(), &branch)
        .await
        .unwrap();

    assert!(
        client
            .post(&format!("/api/sessions/{id}/recover"), json!({}))
            .await
            .is_err(),
        "a deleted kept branch makes recovery fail"
    );
    let view = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(view["status"], "archived");
    assert!(!Path::new(&work_dir).exists());
    assert!(!backend::has_session(&term_session).await);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn respawn_accepts_same_profile_lifetime_and_rejects_recreate() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let profile_body = json!({
        "name": "respawn-lifetime",
        "agent_kind": "shell",
        "protocol": "terminal",
        "mode": "auto",
        "class": "interactive",
        "mcp_access": { "mode": "none", "groups": [] }
    });
    let profile = client
        .post("/api/profiles/create", profile_body.clone())
        .await
        .unwrap();
    client
        .post(
            "/api/profiles/env/set",
            json!({ "profile": "respawn-lifetime", "name": "LIFETIME_TOKEN", "value": "at-launch" }),
        )
        .await
        .unwrap();

    let recoverable = client
        .post(
            "/api/sessions",
            json!({
                "goal": "recover lifetime",
                "cwd": ts.cwd(),
                "profile": "respawn-lifetime"
            }),
        )
        .await
        .unwrap();
    let adoptable = client
        .post(
            "/api/sessions",
            json!({
                "goal": "adopt lifetime",
                "cwd": ts.cwd(),
                "profile": "respawn-lifetime"
            }),
        )
        .await
        .unwrap();
    let recover_id = recoverable["id"].as_str().unwrap();
    let adopt_id = adoptable["id"].as_str().unwrap();
    let adopt_term = adoptable["term_session"].as_str().unwrap();
    let current = client
        .post("/api/profiles/get", json!({ "name": "respawn-lifetime" }))
        .await
        .unwrap();
    let edited = client
        .post(
            "/api/profiles/update",
            json!({
                "name": "respawn-lifetime",
                "description": "same lifetime edit after launch",
                "agent_kind": "shell",
                "protocol": "terminal",
                "mode": "auto",
                "class": "interactive",
                "mcp_access": { "mode": "none", "groups": [] },
                "expected_revision": current["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(edited["lifetime"], profile["lifetime"]);
    let rotated = client
        .post(
            "/api/profiles/env/set",
            json!({ "profile": "respawn-lifetime", "name": "LIFETIME_TOKEN", "value": "rotated-after-launch" }),
        )
        .await
        .unwrap();
    assert_eq!(rotated["lifetime"], profile["lifetime"]);
    client
        .post(&format!("/api/sessions/{recover_id}/archive"), json!({}))
        .await
        .unwrap();
    backend::kill_session(adopt_term).await.unwrap();
    client
        .patch(
            &format!("/api/sessions/{adopt_id}"),
            json!({ "status": "error" }),
        )
        .await
        .unwrap();
    client
        .post(
            "/api/profiles/delete",
            json!({ "name": "respawn-lifetime" }),
        )
        .await
        .unwrap();

    let recovered = client
        .post(&format!("/api/sessions/{recover_id}/recover"), json!({}))
        .await
        .unwrap();
    assert_eq!(recovered["profile_lifetime"], profile["lifetime"]);
    let adopted = client
        .post(&format!("/api/sessions/{adopt_id}/adopt"), json!({}))
        .await
        .unwrap();
    assert_eq!(adopted["profile_lifetime"], profile["lifetime"]);

    client
        .post(&format!("/api/sessions/{recover_id}/archive"), json!({}))
        .await
        .unwrap();
    backend::kill_session(adopt_term).await.unwrap();
    client
        .patch(
            &format!("/api/sessions/{adopt_id}"),
            json!({ "status": "error" }),
        )
        .await
        .unwrap();
    let replacement = client
        .post("/api/profiles/create", profile_body)
        .await
        .unwrap();
    assert_ne!(replacement["lifetime"], profile["lifetime"]);
    client
        .post(
            "/api/profiles/env/set",
            json!({ "profile": "respawn-lifetime", "name": "LIFETIME_TOKEN", "value": "replacement-lifetime" }),
        )
        .await
        .unwrap();

    for path in [
        format!("/api/sessions/{recover_id}/recover"),
        format!("/api/sessions/{adopt_id}/adopt"),
    ] {
        let response = reqwest::Client::new()
            .post(format!("http://{}{}", ts.addr, path))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unavailable profile lifetime"));
    }
}

/// Plant the durable trace of an operation that died mid-teardown: the marker is
/// published, but its owner is a pid in a namespace that no longer exists — a
/// killed server, or an archive that ran inside the session container it was
/// tearing down. `begin_transition` stamps the running process, so the owner is
/// rewritten afterwards.
async fn plant_abandoned_transition(ts: &TestServer, id: &str, transition: &str, step: &str) {
    assert!(
        loom::session::begin_transition(&ts.state.db, id, transition, step)
            .await
            .unwrap()
    );
    sqlx::query("UPDATE sessions SET lifecycle_transition_owner_pid = ? WHERE id = ?")
        .bind(i64::from(i32::MAX))
        .bind(id)
        .execute(&ts.state.db)
        .await
        .unwrap();
}

/// A person must always be able to archive a session. An abandoned marker used
/// to refuse every archive and adopt until the server restarted, because only
/// startup reconciled interrupted transitions.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_takes_over_a_transition_whose_owner_is_gone() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "stuck archiving", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();
    plant_abandoned_transition(&ts, &id, "archiving", "Stopping agent").await;

    client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();

    let detail = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(detail["status"], "archived");
    assert!(detail["transition"].is_null(), "{detail}");
    assert!(
        !Path::new(&work_dir).exists(),
        "the take-over archive should still tear the worktree down"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

/// The same recovery runs on the monitor's cadence, so a session left mid-archive
/// finishes archiving on its own rather than waiting for the next restart.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciliation_finishes_an_abandoned_archive_without_a_restart() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "abandoned mid-archive", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    plant_abandoned_transition(&ts, &id, "archiving", "Stopping agent").await;

    loom::lifecycle::reconcile_interrupted_transitions(&ts.state).await;

    let detail = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(detail["status"], "archived");
    assert!(detail["transition"].is_null(), "{detail}");

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

/// A transition this process still owns is live work: a concurrent archive must
/// keep refusing it rather than tearing down a session mid-operation.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_still_refuses_a_transition_owned_by_this_server() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "busy transitioning", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    assert!(
        loom::session::begin_transition(&ts.state.db, &id, "archiving", "Stopping agent")
            .await
            .unwrap()
    );

    let error = client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already archiving"), "{error}");

    loom::session::clear_transition(&ts.state.db, &id, "archiving")
        .await
        .unwrap();
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}
