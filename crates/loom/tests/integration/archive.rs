//! Archiving a session tears down its tmux session and worktree but — unlike
//! delete — keeps the session row (marked `archived`), the git branch, and the
//! weaver history, and clears the attention flag.

use std::path::Path;

use serde_json::json;
use serial_test::serial;

use loom::tmux;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_keeps_branch_and_history() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let arch = client
        .post(
            "/api/sessions",
            json!({
                "goal": "archive me",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let arch_id = arch["id"].as_str().unwrap().to_string();
    let arch_session = arch["tmux_session"].as_str().unwrap().to_string();
    let arch_work_dir = arch["work_dir"].as_str().unwrap().to_string();
    assert!(
        tmux::has_session(&arch_session).await,
        "archive session missing"
    );
    assert!(
        Path::new(&arch_work_dir).exists(),
        "archive worktree missing"
    );

    // Flag the session for attention; archiving must clear it (a torn-down
    // workstream can't still "need me"). The recorded `attention` event (with
    // its `source: manual` marker) doubles as branch history we expect to
    // survive the archive.
    client
        .patch(
            &format!("/api/sessions/{arch_id}"),
            json!({ "attention": "attention", "description": "Waiting for input" }),
        )
        .await
        .unwrap();

    let res = client
        .post(&format!("/api/sessions/{arch_id}/archive"), json!({}))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(
        !tmux::has_session(&arch_session).await,
        "archive should kill the tmux session"
    );
    assert!(
        !Path::new(&arch_work_dir).exists(),
        "archive should remove the worktree"
    );

    // The session row persists, now terminal/`archived`.
    let view = client
        .get(&format!("/api/sessions/{arch_id}"))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
    // Archiving cleared the attention level so the dashboard stops flagging it.
    // The message (description) is kept as history.
    assert_eq!(
        view["branch"]["attention"], "ok",
        "archive should clear attention"
    );
    // The git branch is left intact for future reference.
    assert!(
        weaver_core::git::branch_exists(ts.repo_path(), "weaver/archive-me").await,
        "archive must not delete the branch"
    );
    // The branch event history survives the archive (unlike delete): the
    // pre-archive manual attention event is still in the log.
    let log = client
        .get(&format!("/api/sessions/{arch_id}/log"))
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&log).unwrap().contains("manual"),
        "branch history should survive archive"
    );

    // An archived session can still be fully removed afterwards.
    client
        .delete(&format!("/api/sessions/{arch_id}"))
        .await
        .unwrap();
}
