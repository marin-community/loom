//! Archiving a session tears down its terminal session and worktree but — unlike
//! delete — keeps the session row (marked `archived`), the git branch, and the
//! weaver history, and clears the attention tag.

use std::path::Path;

use serde_json::json;
use serial_test::serial;

use loom::backend;

use crate::fixtures::{branch_tag, plant_claude_transcript, HomeGuard, TestServer};

/// Archiving captures the agent's conversation log: it locates the Claude Code
/// transcript for the worktree (under `~/.claude/projects/<munged-cwd>/`),
/// normalizes it, and writes a rendered `chat.md` and an iris `chat.json` under
/// the configured session log dir — all before the worktree is torn down.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_captures_the_conversation_log() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Point HOME (transcript source) and the log dir (capture sink) at temp dirs.
    let home = tempfile::tempdir().unwrap();
    let _home_guard = HomeGuard::set(home.path());
    let logs = tempfile::tempdir().unwrap();

    let sess = client
        .post(
            "/api/sessions",
            json!({ "goal": "log me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();

    // Set the capture sink via the settings API.
    client
        .patch(
            "/api/settings",
            json!({ "session.log_dir": logs.path().to_string_lossy() }),
        )
        .await
        .unwrap();

    // Plant a Claude transcript where the agent would have written it.
    plant_claude_transcript(
        home.path(),
        &work_dir,
        "implement the thing",
        "Done — shipped it.",
    );

    let res = client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    let branch = res["branch"].as_str().unwrap();
    let slug = branch.replace('/', "-");

    // Both the rendered markdown and the normalized iris JSON are written.
    let md = std::fs::read_to_string(logs.path().join(&slug).join("chat.md"))
        .expect("chat.md should be captured on archive");
    assert!(md.contains("# Conversation log"), "rendered markdown: {md}");
    assert!(
        md.contains("implement the thing"),
        "user turn captured: {md}"
    );
    assert!(
        md.contains("Done — shipped it."),
        "assistant turn captured: {md}"
    );

    let raw_json = std::fs::read_to_string(logs.path().join(&slug).join("chat.json"))
        .expect("chat.json should be captured on archive");
    let iris: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
    assert_eq!(iris["source"], "claude");
    assert_eq!(iris["messages"].as_array().unwrap().len(), 2);
}

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
    let tracking_issue = arch["tracking_issue"].as_i64().unwrap();
    let arch_session = arch["term_session"].as_str().unwrap().to_string();
    let arch_work_dir = arch["work_dir"].as_str().unwrap().to_string();
    assert!(
        backend::has_session(&arch_session).await,
        "archive session missing"
    );
    assert!(
        Path::new(&arch_work_dir).exists(),
        "archive worktree missing"
    );

    // Flag the session for attention; archiving must clear it (a torn-down
    // workstream can't still "need me"). The recorded `tag` event (authored
    // `manual`) doubles as branch history we expect to survive the archive. The
    // message (description) is a separate branch field, patched alongside.
    client
        .put(
            &format!("/api/sessions/{arch_id}/tags/attention"),
            json!({ "value": "attention", "by": "manual" }),
        )
        .await
        .unwrap();
    // A watch's typed loud mark (a non-well-known key on the ladder): archiving
    // must clear it too — loudness is value-driven, not a fixed key set.
    client
        .put(
            &format!("/api/sessions/{arch_id}/tags/review"),
            json!({ "value": "attention", "by": "status-check" }),
        )
        .await
        .unwrap();
    // The soothing `idle` mark is quiet (not on the loud ladder) but is still a
    // lifecycle signal a torn-down workstream shouldn't carry: archiving clears
    // it too.
    client
        .put(
            &format!("/api/sessions/{arch_id}/tags/idle"),
            json!({ "value": "idle", "by": "agent" }),
        )
        .await
        .unwrap();
    // The per-session opt-out gates automatic retention only. An explicit
    // operator Archive must still tear the session down.
    client
        .put(
            &format!("/api/sessions/{arch_id}/tags/auto-archive"),
            json!({ "value": "disabled", "by": "manual" }),
        )
        .await
        .unwrap();
    client
        .patch(
            &format!("/api/sessions/{arch_id}"),
            json!({ "description": "Waiting for input" }),
        )
        .await
        .unwrap();

    let res = client
        .post(&format!("/api/sessions/{arch_id}/archive"), json!({}))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(
        !backend::has_session(&arch_session).await,
        "archive should kill the terminal session"
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
    let issues = client.get("/api/issues?automation=true").await.unwrap();
    let issue = issues
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["id"].as_i64() == Some(tracking_issue))
        .expect("the archived session's tracking issue survives");
    assert!(
        issue["claimed_branch"].is_null(),
        "an archived session must not own any issues"
    );
    // Archiving cleared the attention tag so the dashboard stops flagging it
    // (absence is the calm state). The message (description) is kept as history.
    assert!(
        branch_tag(&view, "attention").is_none(),
        "archive should clear the attention tag"
    );
    assert!(
        branch_tag(&view, "review").is_none(),
        "archive should clear a watch's typed loud mark too"
    );
    assert!(
        branch_tag(&view, "idle").is_none(),
        "archive should clear the soothing idle mark too"
    );
    assert_eq!(
        branch_tag(&view, "auto-archive").unwrap()["value"],
        "disabled",
        "manual archive ignores and preserves the quiet automatic-retention override"
    );
    assert_eq!(
        view["branch"]["description"], "Waiting for input",
        "archive keeps the status message as history"
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
