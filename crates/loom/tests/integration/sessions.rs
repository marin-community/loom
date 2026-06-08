//! Session lifecycle over the REST API: create → list → recent-repos → delete,
//! plus adoption of an externally-killed session and the no-goal create path.

use serde_json::json;
use serial_test::serial;

use loom::tmux;

use crate::fixtures::{sh, TestServer};

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

/// With an `origin` remote present, a launch that doesn't pin `--base` forks the
/// new branch from the freshly-fetched `origin/<default branch>`, recorded as
/// the branch's base.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_forks_from_fresh_origin_default() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let repo = ts.repo_path();

    // Give the throwaway repo a bare `origin` and publish `main` to it, so the
    // remote-tracking ref + origin/HEAD exist (what `default_base` resolves).
    let remote = tempfile::tempdir().unwrap();
    sh(
        remote.path(),
        "git",
        &["init", "-q", "--bare", "-b", "main"],
    );
    let remote_url = remote.path().to_string_lossy().to_string();
    sh(repo, "git", &["remote", "add", "origin", &remote_url]);
    sh(repo, "git", &["push", "-q", "origin", "main"]);
    sh(repo, "git", &["fetch", "-q", "origin"]);
    sh(repo, "git", &["remote", "set-head", "origin", "main"]);

    let ws = client
        .post(
            "/api/sessions",
            json!({ "goal": "fork from fresh main", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    assert_eq!(
        ws["branch"]["base_branch"], "origin/main",
        "launch should fork from the fetched origin default, got {ws}"
    );

    let id = ws["id"].as_str().unwrap().to_string();
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
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

/// A delegated launch records its launcher as the session's tree parent
/// (`parent_id`); a top-level launch has none. The link is stored on the session
/// row at create time, so it survives a re-list unchanged.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_records_its_launcher_as_tree_parent() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let cwd = ts.cwd();

    // A top-level (human) launch has no parent.
    let parent = client
        .post(
            "/api/sessions",
            json!({ "goal": "parent work", "cwd": cwd, "agent": "shell", "name": "parent" }),
        )
        .await
        .unwrap();
    let parent_branch_id = parent["branch"]["id"].as_str().unwrap().to_string();
    assert!(
        parent["parent_id"].is_null(),
        "a top-level launch has no tree parent"
    );

    // A delegated launch names the parent branch; its session points back at it.
    let child = client
        .post(
            "/api/sessions",
            json!({
                "goal": "child work",
                "cwd": cwd,
                "agent": "shell",
                "name": "child",
                "parent_branch": parent_branch_id,
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    assert_eq!(
        child["parent_id"].as_str(),
        Some(parent_branch_id.as_str()),
        "the child's tree parent is the launching branch"
    );

    // Stored, not recomputed: the link is still there on a plain list.
    let list = client.get("/api/sessions").await.unwrap();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == child_id.as_str())
        .expect("child session in list");
    assert_eq!(row["parent_id"].as_str(), Some(parent_branch_id.as_str()));

    client
        .delete(&format!("/api/sessions/{child_id}"))
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap();
    client
        .delete(&format!("/api/sessions/{parent_id}"))
        .await
        .unwrap();
}
