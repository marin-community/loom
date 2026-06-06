//! Session lifecycle over the REST API: create → list → recent-repos → delete,
//! plus adoption of an externally-killed session and the no-goal create path.

use serde_json::json;
use serial_test::serial;

use loom::tmux;

use crate::fixtures::TestServer;

/// Creating a session provisions a worktree + tmux session and records the repo;
/// deleting it tears the tmux session down and releases the repo's active count.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_lists_and_tears_down() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "integration test goal",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let session = ws["tmux_session"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();
    let repo_root = ws["branch"]["repo_root"].as_str().unwrap().to_string();

    assert!(
        std::path::Path::new(&work_dir).join(".git").exists(),
        "worktree was not created"
    );
    assert!(
        work_dir.ends_with("/.worktrees/integration-test-goal"),
        "worktree should live inside the repo at .worktrees/<slug>, got {work_dir}"
    );
    assert_eq!(ws["branch"]["branch"], "weaver/integration-test-goal");
    assert_eq!(
        ws["branch"]["title"], "integration test goal",
        "title derived from goal"
    );
    // The launch hands back a tracking issue id — the caller's handle on it.
    assert!(
        ws["tracking_issue"].as_i64().is_some(),
        "launch returns a tracking issue id, got {ws}"
    );
    assert!(tmux::has_session(&session).await, "tmux session missing");

    let list = client.get("/api/sessions").await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let recent = client.get("/api/repos/recent").await.unwrap();
    let recent = recent.as_array().unwrap();
    assert_eq!(
        recent.len(),
        1,
        "repo should be recorded after first session"
    );
    assert_eq!(recent[0]["repo_root"], repo_root);
    assert_eq!(recent[0]["active_branches"], 1);

    // Deleting the session tears down the tmux session and the DB row.
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
    assert!(
        !tmux::has_session(&session).await,
        "tmux session was not killed"
    );
    let list = client.get("/api/sessions").await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);

    // The repo outlives its sessions, now with no active branches.
    let recent = client.get("/api/repos/recent").await.unwrap();
    let recent = recent.as_array().unwrap();
    assert_eq!(recent.len(), 1, "recent repo should outlive its sessions");
    assert_eq!(recent[0]["repo_root"], repo_root);
    assert_eq!(recent[0]["active_branches"], 0);
}

/// A session can be created with no goal at all — just a title.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_session_has_no_goal() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let bare = client
        .post(
            "/api/sessions",
            json!({
                "cwd": ts.cwd(),
                "title": "no goal here",
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    assert_eq!(bare["branch"]["goal"], "", "goal should be empty");
    assert_eq!(bare["branch"]["title"], "no goal here");

    let bare_id = bare["id"].as_str().unwrap().to_string();
    client
        .delete(&format!("/api/sessions/{bare_id}"))
        .await
        .unwrap();
}

/// Adoption recovers a session whose tmux server was killed out from under loom:
/// it recreates the tmux session; adopting a live one is rejected.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_recreates_killed_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "adopt me",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let session = ws["tmux_session"].as_str().unwrap().to_string();

    tmux::kill_session(&session).await.unwrap();
    assert!(
        !tmux::has_session(&session).await,
        "session should be gone after kill"
    );

    let adopted = client
        .post(&format!("/api/sessions/{id}/adopt"), json!({}))
        .await
        .unwrap();
    assert_eq!(
        adopted["status"], "launching",
        "adopt sets status launching"
    );
    assert!(
        tmux::has_session(&session).await,
        "adopt should recreate the tmux session"
    );
    assert!(
        client
            .post(&format!("/api/sessions/{id}/adopt"), json!({}))
            .await
            .is_err(),
        "adopting a live session should fail"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}
