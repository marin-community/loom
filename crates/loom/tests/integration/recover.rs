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
