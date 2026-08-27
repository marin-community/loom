//! Recovering an archived session is the inverse of archiving: it rebuilds the
//! worktree from the kept branch and resumes the agent, flipping the row back out
//! of the terminal `archived` state and into the live fleet — structured like
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
            "/api/sessions/launch",
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
        .post("/api/sessions/archive", json!({ "session": id }))
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
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
    let channel = client
        .post("/api/channels/get", json!({ "channel": id }))
        .await
        .unwrap();
    assert_eq!(channel["state"], "archived");

    // Recover rebuilds the worktree and resumes the agent at the same path.
    let rec = client
        .post("/api/sessions/recover", json!({ "session": id }))
        .await
        .unwrap();
    // The row is live again (shell is hookless, so it comes up `running`), on the
    // same worktree path and terminal session.
    assert_eq!(rec["status"], "running");
    assert_eq!(rec["work_dir"], work_dir);
    assert_eq!(rec["term_session"], term_session);
    let channel = client
        .post("/api/channels/get", json!({ "channel": id }))
        .await
        .unwrap();
    assert_eq!(channel["state"], "open");
    let messages = client
        .post(
            "/api/channels/messages/list",
            json!({ "channel": id, "kinds": [] }),
        )
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
    let fleet = client.post("/api/sessions/list", json!({})).await.unwrap();
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
            "/api/sessions/launch",
            json!({ "goal": "archive adopt race", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap();

    client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();
    let error = client
        .post("/api/sessions/adopt", json!({ "session": id }))
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("recover"), "{message}");
    assert!(!message.contains("no longer exists on disk"), "{message}");

    let recovered = client
        .post("/api/sessions/recover", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(recovered["status"], "running");
    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transition_stage_is_visible_in_detail_and_fleet_views() {
    let ts = TestServer::start().await;
    let sess = ts
        .client
        .post(
            "/api/sessions/launch",
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

    let detail = ts
        .client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(detail["status"], "running");
    assert_eq!(detail["transition"]["kind"], "archiving");
    assert_eq!(detail["transition"]["step"], "Removing worktree");
    assert!(!detail["transition"]["started_at"]
        .as_str()
        .unwrap()
        .is_empty());

    let fleet = ts
        .client
        .post("/api/sessions/summary/list", json!({}))
        .await
        .unwrap();
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
        .post("/api/sessions/delete", json!({ "session": id }))
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
            "/api/sessions/launch",
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
        .post("/api/sessions/recover", json!({ "session": id }))
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
            "/api/sessions/launch",
            json!({ "goal": "cannot recover", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let branch = sess["branch"]["branch"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();
    weaver_core::git::delete_branch(ts.repo_path(), &branch)
        .await
        .unwrap();

    assert!(
        client
            .post("/api/sessions/recover", json!({ "session": id }))
            .await
            .is_err(),
        "a deleted kept branch makes recovery fail"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
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
            "/api/sessions/launch",
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
            "/api/sessions/launch",
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
        .post("/api/sessions/archive", json!({ "session": recover_id }))
        .await
        .unwrap();
    backend::kill_session(adopt_term).await.unwrap();
    client
        .post(
            "/api/sessions/update",
            json!({ "session": adopt_id, "status": "error" }),
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
        .post("/api/sessions/recover", json!({ "session": recover_id }))
        .await
        .unwrap();
    assert_eq!(recovered["profile_lifetime"], profile["lifetime"]);
    let adopted = client
        .post("/api/sessions/adopt", json!({ "session": adopt_id }))
        .await
        .unwrap();
    assert_eq!(adopted["profile_lifetime"], profile["lifetime"]);

    client
        .post("/api/sessions/archive", json!({ "session": recover_id }))
        .await
        .unwrap();
    backend::kill_session(adopt_term).await.unwrap();
    client
        .post(
            "/api/sessions/update",
            json!({ "session": adopt_id, "status": "error" }),
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

    for (path, session) in [
        ("/api/sessions/recover", recover_id),
        ("/api/sessions/adopt", adopt_id),
    ] {
        let response = reqwest::Client::new()
            .post(format!("http://{}{}", ts.addr, path))
            .json(&json!({ "session": session }))
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

/// A person must always be able to archive a session, even one stuck behind an
/// abandoned transition marker — recovering it must not require a server
/// restart.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_takes_over_a_transition_whose_owner_is_gone() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "stuck archiving", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();
    plant_abandoned_transition(&ts, &id, "archiving", "Stopping agent").await;

    client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();

    let detail = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(detail["status"], "archived");
    assert!(detail["transition"].is_null(), "{detail}");
    assert!(
        !Path::new(&work_dir).exists(),
        "the take-over archive should still tear the worktree down"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
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
            "/api/sessions/launch",
            json!({ "goal": "abandoned mid-archive", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    plant_abandoned_transition(&ts, &id, "archiving", "Stopping agent").await;

    loom::lifecycle::reconcile_interrupted_transitions(&ts.state).await;

    let detail = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(detail["status"], "archived");
    assert!(detail["transition"].is_null(), "{detail}");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
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
            "/api/sessions/launch",
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
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already archiving"), "{error}");

    loom::session::clear_transition(&ts.state.db, &id, "archiving")
        .await
        .unwrap();
    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}
