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

/// Recovering a session whose terminal is still live is refused — recover is for
/// a torn-down session, not a hijack of a running one (the same guard adopt uses).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_refuses_a_live_session() {
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

    let err = client
        .post(&format!("/api/sessions/{id}/recover"), json!({}))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("running terminal"),
        "recover should reject a session with a live terminal: {err}"
    );
}
