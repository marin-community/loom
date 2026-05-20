//! End-to-end test driving a real server with a shell-backed workspace.
//! Requires `git` and `tmux` on PATH.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use tokio::net::TcpListener;
use weaver::client::Client;
use weaver::events::EventBus;
use weaver::web::AppState;
use weaver::{db, server, tmux};

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_lifecycle() {
    // Isolate all weaver state in a temp dir.
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("WEAVER_HOME", home.path());

    // Build a throwaway git repo with a single commit on `main`.
    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git", &["init", "-b", "main"]);
    sh(repo.path(), "git", &["config", "user.email", "t@t.test"]);
    sh(repo.path(), "git", &["config", "user.name", "Test"]);
    std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
    sh(repo.path(), "git", &["add", "."]);
    sh(repo.path(), "git", &["commit", "-m", "init"]);

    // Start the server on a random port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let pool = db::connect(&db::default_db_path()).await.unwrap();
    let state = AppState {
        db: pool,
        bus: EventBus::new(),
        addr: addr.to_string(),
    };
    tokio::spawn(server::serve(state, listener));

    std::env::set_var("WEAVER_API", format!("http://{addr}"));
    let client = Client::new();
    for _ in 0..60 {
        if client.get("/api/health").await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Create a workspace backed by a plain shell.
    let ws = client
        .post(
            "/api/workspaces",
            json!({
                "goal": "integration test goal",
                "cwd": repo.path().to_string_lossy(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let session = ws["tmux_session"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();
    let repo_root = ws["repo_root"].as_str().unwrap().to_string();

    // Worktree, branch and tmux session all exist.
    assert!(
        std::path::Path::new(&work_dir).join(".git").exists(),
        "worktree was not created"
    );
    assert!(
        work_dir.ends_with("/.worktrees/integration-test-goal"),
        "worktree should live inside the repo at .worktrees/<slug>, got {work_dir}"
    );
    assert_eq!(ws["branch"], "weaver/integration-test-goal");
    assert_eq!(ws["title"], "integration test goal", "title derived from goal");
    assert!(tmux::has_session(&session).await, "tmux session missing");

    // It shows up in the listing.
    let list = client.get("/api/workspaces").await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // Creating the workspace recorded its repo as recently used.
    let recent = client.get("/api/repos/recent").await.unwrap();
    let recent = recent.as_array().unwrap();
    assert_eq!(recent.len(), 1, "repo should be recorded after first workspace");
    assert_eq!(recent[0]["repo_root"], repo_root);
    assert_eq!(recent[0]["active_workspaces"], 1);

    // Text sent to the workspace reaches the pane.
    client
        .post(
            &format!("/api/workspaces/{id}/send"),
            json!({ "text": "echo WEAVER_MARKER_123" }),
        )
        .await
        .unwrap();
    let mut found = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let pane = client
            .get(&format!("/api/workspaces/{id}/pane"))
            .await
            .unwrap();
        if pane["content"]
            .as_str()
            .unwrap_or("")
            .contains("WEAVER_MARKER_123")
        {
            found = true;
            break;
        }
    }
    assert!(found, "sent text never appeared in the pane");

    // A hook flips the workspace status.
    client
        .post("/api/hook", json!({ "workspace": id, "event": "working" }))
        .await
        .unwrap();
    let ws = client.get(&format!("/api/workspaces/{id}")).await.unwrap();
    assert_eq!(ws["status"], "working");

    // The diff endpoint responds (no changes yet).
    let diff = client
        .get(&format!("/api/workspaces/{id}/diff"))
        .await
        .unwrap();
    assert!(diff["patch"].is_string());

    // Adoption: kill the tmux session out from under weaver (as a reboot
    // would), then adopt the workspace and confirm the session is recreated.
    tmux::kill_session(&session).await.unwrap();
    assert!(
        !tmux::has_session(&session).await,
        "session should be gone after kill"
    );
    let adopted = client
        .post(&format!("/api/workspaces/{id}/adopt"), json!({}))
        .await
        .unwrap();
    assert_eq!(adopted["status"], "launching", "adopt sets status launching");
    assert!(
        tmux::has_session(&session).await,
        "adopt should recreate the tmux session"
    );
    // Adopting a workspace that already has a live session is rejected.
    assert!(
        client
            .post(&format!("/api/workspaces/{id}/adopt"), json!({}))
            .await
            .is_err(),
        "adopting a live workspace should fail"
    );

    // A workspace can be created with no goal at all (agent starts unprompted).
    let bare = client
        .post(
            "/api/workspaces",
            json!({
                "cwd": repo.path().to_string_lossy(),
                "title": "no goal here",
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    assert_eq!(bare["goal"], "", "goal should be empty");
    assert_eq!(bare["title"], "no goal here");
    let bare_id = bare["id"].as_str().unwrap().to_string();
    client
        .delete(&format!("/api/workspaces/{bare_id}"))
        .await
        .unwrap();

    // Deleting the workspace tears down the tmux session and the DB row.
    client
        .delete(&format!("/api/workspaces/{id}"))
        .await
        .unwrap();
    assert!(
        !tmux::has_session(&session).await,
        "tmux session was not killed"
    );
    let list = client.get("/api/workspaces").await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);

    // The repo is still remembered after every workspace in it is gone — that
    // is what lets the dashboard offer it for the next session.
    let recent = client.get("/api/repos/recent").await.unwrap();
    let recent = recent.as_array().unwrap();
    assert_eq!(recent.len(), 1, "recent repo should outlive its workspaces");
    assert_eq!(recent[0]["repo_root"], repo_root);
    assert_eq!(recent[0]["active_workspaces"], 0);
}
