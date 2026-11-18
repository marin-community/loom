//! Archiving a session tears down its terminal session and worktree but — unlike
//! delete — keeps the session row (marked `archived`), the git branch, and the
//! weaver history, and clears the attention tag.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serial_test::serial;
use tokio::io::AsyncWriteExt;

use loom::backend;

use crate::fixtures::{branch_tag, plant_claude_transcript, HomeGuard, TestServer};
use weaver_api::operations::branches;

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
            "/api/sessions/launch",
            json!({ "goal": "log me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();

    // Set the capture sink via the settings API.
    client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "session.log_dir": logs.path().to_string_lossy() } }),
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
        .post("/api/sessions/archive", json!({ "session": id }))
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
async fn archive_survives_request_disconnect() {
    let ts = TestServer::start().await;
    let created = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "archive after disconnect", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let work_dir = created["work_dir"].as_str().unwrap().to_string();

    // A legacy child, linked only by the parent's branch id — the linkage a
    // retry could not rediscover once the parent is archived — so the test
    // proves the requested sweep itself survives the disconnect.
    sqlx::query("UPDATE sessions SET status = 'done' WHERE id = ?")
        .bind(&id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let child = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "legacy child of the disconnected archive",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": created["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();

    // A raw connection lets the test close the transport without waiting for a
    // response, matching a browser navigation or reverse-proxy disconnect.
    let mut connection = tokio::net::TcpStream::connect(ts.addr).await.unwrap();
    let body = json!({ "session": id, "children": true }).to_string();
    let request = format!(
        "POST /api/sessions/archive HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        ts.addr,
        body.len(),
        body
    );
    connection.write_all(request.as_bytes()).await.unwrap();
    connection.flush().await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let session = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if session.lifecycle_transition.as_deref() == Some("archiving") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("archive should start");

    drop(connection);

    let archived = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let session = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if session.status == "archived" {
                break session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("archive should finish after its request disconnects");

    assert!(archived.lifecycle_transition.is_none());
    assert!(!Path::new(&work_dir).exists());

    let child = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let session = loom::session::get(&ts.state.db, &child_id)
                .await
                .unwrap()
                .unwrap();
            if session.status == "archived" {
                break session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the requested child sweep survives the disconnect too");
    assert!(child.lifecycle_transition.is_none());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_keeps_branch_and_history() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let arch = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "archive me",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let arch_id = arch["id"].as_str().unwrap().to_string();
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
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "attention", "value": "attention", "by": "manual", "session": arch_id }),
        )
        .await
        .unwrap();
    // A watch's typed loud mark (a non-well-known key on the ladder): archiving
    // must clear it too — loudness is value-driven, not a fixed key set.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "review", "value": "attention", "by": "status-check", "session": arch_id }),
        )
        .await
        .unwrap();
    // The soothing `idle` mark is quiet (not on the loud ladder) but is still a
    // lifecycle signal a torn-down workstream shouldn't carry: archiving clears
    // it too.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "idle", "value": "idle", "by": "agent", "session": arch_id }),
        )
        .await
        .unwrap();
    // The per-session opt-out gates automatic retention only. An explicit
    // operator Archive must still tear the session down.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "auto-archive", "value": "disabled", "by": "manual", "session": arch_id }),
        )
        .await
        .unwrap();
    client
        .post(
            "/api/sessions/update",
            json!({ "description": "Waiting for input", "session": arch_id }),
        )
        .await
        .unwrap();

    let res = client
        .post("/api/sessions/archive", json!({ "session": arch_id }))
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
        .post("/api/sessions/get", json!({ "session": arch_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
    let channel = client
        .post("/api/channels/get", json!({ "channel": arch_id }))
        .await
        .unwrap();
    assert_eq!(channel["state"], "archived");
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
    // The branch event history survives the archive (unlike delete). The typed
    // client uses the branch-owned operation; `sessions.events.list` wraps the
    // same handler keyed by session and must remain an exact compatibility
    // alias.
    let branch_log = client
        .invoke::<branches::events::list::Op>(&branches::events::list::Input {
            branch: arch_id.to_string(),
        })
        .await
        .unwrap();
    let session_log = client
        .post("/api/sessions/events/list", json!({ "session": arch_id }))
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&branch_log)
            .unwrap()
            .contains("manual"),
        "branch history should survive archive"
    );
    assert_eq!(
        serde_json::to_value(&branch_log).unwrap(),
        session_log,
        "sessions.events.list must remain a compatibility alias for branches.events.list"
    );

    // An archived session can still be fully removed afterwards.
    client
        .post("/api/sessions/delete", json!({ "session": arch_id }))
        .await
        .unwrap();
}

/// A link minted with the branch id (`artifact_url` takes any session key) must
/// keep opening the session page after that session is archived: a branch key
/// resolves to the branch's newest session once none is live. Every verb resolves
/// keys the same way, so one that acts on the session reaches it by branch key
/// just as it already does by session id.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_key_still_reads_its_archived_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let launched = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "link me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let session_id = launched["id"].as_str().unwrap().to_string();
    let branch_id = launched["branch"]["id"].as_str().unwrap().to_string();
    assert_ne!(session_id, branch_id);

    client
        .post("/api/sessions/archive", json!({ "session": session_id }))
        .await
        .unwrap();

    let view = client
        .post("/api/sessions/get", json!({ "session": branch_id }))
        .await
        .unwrap();
    assert_eq!(view["id"], session_id);
    assert_eq!(view["status"], "archived");
    assert_eq!(view["branch"]["id"], branch_id);

    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "attention", "value": "attention", "by": "manual", "session": branch_id }),
        )
        .await
        .unwrap();
    let by_id = client
        .post("/api/sessions/get", json!({ "session": session_id }))
        .await
        .unwrap();
    assert_eq!(
        branch_tag(&by_id, "attention").unwrap()["value"],
        "attention"
    );
}

/// The `session.archive_children` setting decides for callers that stay
/// silent; an explicit operand overrides it in both directions (explicit
/// true with the setting off is covered above).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_archive_children_setting_defaults_a_silent_request() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "session.archive_children": true } }),
        )
        .await
        .unwrap();

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate silently", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "ride along by default",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();

    let res = client
        .post("/api/sessions/archive", json!({ "session": parent_id }))
        .await
        .unwrap();
    assert_eq!(
        res["children_archived"], 1,
        "a silent request follows the setting"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": child_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate alone", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "stay behind an explicit false",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();

    let res = client
        .post(
            "/api/sessions/archive",
            json!({ "session": parent_id, "children": false }),
        )
        .await
        .unwrap();
    assert_eq!(
        res["children_archived"], 0,
        "an explicit false wins over the setting"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": child_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running");
}

/// Automatic archives follow the setting through `auto_archive`, and a swept
/// child's own `auto-archive: disabled` opt-out skips it together with its
/// subtree — the tag exists to stop automatic archival of that session.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_archive_follows_the_setting_and_child_opt_outs() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "session.archive_children": true } }),
        )
        .await
        .unwrap();

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate for the reaper", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "opt out of automatic archival",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let grandchild = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "survive inside the opted-out subtree",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": child["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let grandchild_id = grandchild["id"].as_str().unwrap().to_string();
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "auto-archive", "value": "disabled", "by": "manual", "session": child_id }),
        )
        .await
        .unwrap();

    let (session, branch) = loom::session::with_branch(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    let outcome = loom::lifecycle::auto_archive(&ts.state, &session, &branch)
        .await
        .unwrap()
        .expect("the root carries no opt-out");
    assert_eq!(
        outcome.children_archived, 0,
        "the opted-out child takes its subtree with it"
    );
    for id in [&child_id, &grandchild_id] {
        let view = client
            .post("/api/sessions/get", json!({ "session": id }))
            .await
            .unwrap();
        assert_eq!(view["status"], "running", "{id} keeps working");
    }

    // Without the tag the same sweep takes the child along.
    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate once more", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "ride along with the reaper",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let (session, branch) = loom::session::with_branch(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    let outcome = loom::lifecycle::auto_archive(&ts.state, &session, &branch)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.children_archived, 1);
    let view = client
        .post("/api/sessions/get", json!({ "session": child_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
}

/// `children: true` takes the whole delegation tree down with the root, and
/// the result reports how many descendants rode along.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_with_children_takes_the_delegation_tree_along() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate the work", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "do the delegated work",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let child_work_dir = child["work_dir"].as_str().unwrap().to_string();
    let grandchild = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "grandchild work",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": child["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let grandchild_id = grandchild["id"].as_str().unwrap().to_string();
    let bystander = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "unrelated work", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let bystander_id = bystander["id"].as_str().unwrap().to_string();

    let res = client
        .post(
            "/api/sessions/archive",
            json!({ "session": parent_id, "children": true }),
        )
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert_eq!(
        res["children_archived"], 2,
        "the child and the grandchild ride along"
    );
    assert!(
        res["warnings"].as_array().unwrap().is_empty(),
        "a clean subtree archives without warnings: {res}"
    );

    for id in [&parent_id, &child_id, &grandchild_id] {
        let view = client
            .post("/api/sessions/get", json!({ "session": id }))
            .await
            .unwrap();
        assert_eq!(view["status"], "archived", "{id} should be archived");
    }
    assert!(
        !Path::new(&child_work_dir).exists(),
        "a riding child's worktree is torn down with it"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": bystander_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running", "an unrelated session stays put");
}

/// Absent `children` follows the `session.archive_children` setting — off by
/// default, so descendants survive the root's archive.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_without_children_leaves_the_delegation_tree_standing() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate only myself", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "survive the parent archive",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();

    let res = client
        .post("/api/sessions/archive", json!({ "session": parent_id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert_eq!(res["children_archived"], 0);

    let view = client
        .post("/api/sessions/get", json!({ "session": child_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running", "the child keeps working");
}

/// A legacy child — linked only by its parent's branch id, the shape a launch
/// produces while the parent is already `done` — still rides along: the
/// subtree is snapshotted before the root archives, while the root still
/// holds its branch.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_done_parents_legacy_child_still_rides_along() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "delegate after finishing", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE sessions SET status = 'done' WHERE id = ?")
        .bind(&parent_id)
        .execute(&ts.state.db)
        .await
        .unwrap();

    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "legacy-linked child",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let row = loom::session::get(&ts.state.db, &child_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.parent_session_id, None,
        "the launch stamps only the branch-id link"
    );
    assert!(row.parent_branch_id.is_some());

    let res = client
        .post(
            "/api/sessions/archive",
            json!({ "session": parent_id, "children": true }),
        )
        .await
        .unwrap();
    assert_eq!(res["children_archived"], 1, "the legacy child is swept");
    let view = client
        .post("/api/sessions/get", json!({ "session": child_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
}

/// Retrying an archived row must not touch a replacement session on the same
/// branch: the archived row no longer owns the branch, its worktree path, its
/// issue claims, or its tags.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrying_an_archived_row_leaves_a_replacement_session_alone() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let first = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "first tenant", "cwd": ts.cwd(), "agent": "shell", "name": "slot" }),
        )
        .await
        .unwrap();
    let first_id = first["id"].as_str().unwrap().to_string();
    let branch_ref = first["branch"]["branch"].as_str().unwrap().to_string();
    client
        .post("/api/sessions/archive", json!({ "session": first_id }))
        .await
        .unwrap();

    // The archived session no longer occupies the slot: a fresh session
    // attaches to the kept branch, re-provisioning the same worktree path.
    let second = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "second tenant",
                "cwd": ts.cwd(),
                "agent": "shell",
                "existing_branch": branch_ref,
            }),
        )
        .await
        .unwrap();
    let second_id = second["id"].as_str().unwrap().to_string();
    let second_work_dir = second["work_dir"].as_str().unwrap().to_string();
    assert_ne!(second_id, first_id);
    assert!(Path::new(&second_work_dir).exists());
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "key": "attention", "value": "attention", "by": "manual", "session": second_id }),
        )
        .await
        .unwrap();

    // The retry of the old, archived row — whose probe now sees a worktree at
    // the path the replacement owns.
    let res = client
        .post("/api/sessions/archive", json!({ "session": first_id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);

    let view = client
        .post("/api/sessions/get", json!({ "session": second_id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running", "the replacement keeps running");
    assert!(
        Path::new(&second_work_dir).exists(),
        "the replacement's worktree is not torn down"
    );
    assert!(
        branch_tag(&view, "attention").is_some(),
        "the replacement's tags are not cleared"
    );

    client
        .post("/api/sessions/delete", json!({ "session": second_id }))
        .await
        .unwrap();
}

/// Archiving an already-archived row is the idempotent success a fleet batch
/// needs: one call's `children` pass can take a row down before another reaches it.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_an_archived_row_is_a_no_op_success() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let parent = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "batch parent", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let child = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "batch child",
                "cwd": ts.cwd(),
                "agent": "shell",
                "parent_branch": parent["branch"]["id"],
            }),
        )
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();

    let res = client
        .post(
            "/api/sessions/archive",
            json!({ "session": parent_id, "children": true }),
        )
        .await
        .unwrap();
    assert_eq!(res["children_archived"], 1);

    // The duplicate: the child's own call, arriving after the parent's pass
    // already archived it.
    let res = client
        .post("/api/sessions/archive", json!({ "session": child_id }))
        .await
        .unwrap();
    assert_eq!(
        res["archived"], true,
        "the no-op is the success the caller wanted"
    );
    assert_eq!(res["children_archived"], 0);
    assert!(
        res["warnings"].as_array().unwrap().is_empty(),
        "nothing is torn down a second time: {res}"
    );
}

/// Re-archiving an archived row re-runs teardown only when leftovers survive:
/// a worktree that outlived its archive is removed by the retry, while a
/// cleanly archived row is the silent no-op the batch dedup needs.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rearchiving_an_archived_row_cleans_surviving_leftovers() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let created = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "leave leftovers", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let work_dir = created["work_dir"].as_str().unwrap().to_string();

    let res = client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(!Path::new(&work_dir).exists());

    std::fs::create_dir_all(&work_dir).unwrap();
    let res = client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(
        !Path::new(&work_dir).exists(),
        "the rerun tears down the surviving worktree"
    );
}
