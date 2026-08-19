//! Branches and intentional work items: branch-claimed issues vs the repo-wide
//! board, claim release on teardown, and attaching a session to a pre-existing
//! git branch (with or without an existing worktree).

use reqwest::StatusCode;
use serde_json::json;
use serial_test::serial;

use weaver_api::operations::issues::{close, reopen, tags};

use crate::fixtures::{branch_tag, branch_tag_value, sh, TestServer};

/// Ordinary launches do not manufacture issues. Hand-created issues are claimed
/// by the branch and show on the repo board; teardown releases the claims but
/// keeps the issues.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_issues_and_repo_board() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let repo_root = ts.cwd();

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "integration test goal",
                "cwd": repo_root,
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let branch_id = ws["branch"]["id"].as_str().unwrap().to_string();

    // Branches endpoint lists this branch with the right metadata.
    let branches = client.post("/api/branches/list", json!({})).await.unwrap();
    let arr = branches.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one branch tracked");
    assert_eq!(arr[0]["branch"], "weaver/integration-test-goal");
    assert_eq!(arr[0]["open_issue_count"], 0);
    let tracking = client
        .post("/api/branches/issues/list", json!({ "branch": branch_id }))
        .await
        .unwrap();
    let tracking = tracking.as_array().unwrap();
    assert!(tracking.is_empty(), "ordinary launch opens no issue");

    // Branch issues are claimed by the branch; routine issue operations use
    // generated resource-grouped endpoints.
    let created = client
        .post(
            "/api/issues/create",
            json!({ "branch": branch_id, "title": "fix it", "body": "details" }),
        )
        .await
        .unwrap();
    let issue_id = created["id"].as_i64().unwrap();
    assert_eq!(created["status"], "open");
    assert_eq!(
        created["claimed_branch"], "weaver/integration-test-goal",
        "a branch issue is claimed by its branch"
    );
    let listed = client
        .post("/api/branches/issues/list", json!({ "branch": branch_id }))
        .await
        .unwrap();
    assert_eq!(
        listed.as_array().unwrap().len(),
        1,
        "only the hand-created work item"
    );
    let branch_view = client
        .post("/api/branches/get", json!({ "branch": branch_id }))
        .await
        .unwrap();
    assert_eq!(branch_view["open_issue_count"], 1);
    // The repo board sees the claimed issue; the unclaimed backlog does not.
    let board = client
        .post(
            "/api/issues/list",
            json!({ "repo_root": repo_root, "all": false }),
        )
        .await
        .unwrap();
    assert_eq!(board.as_array().unwrap().len(), 1);
    let backlog = client
        .post(
            "/api/issues/list",
            json!({ "repo_root": repo_root, "backlog": true, "all": false }),
        )
        .await
        .unwrap();
    assert_eq!(
        backlog.as_array().unwrap().len(),
        0,
        "issue is claimed, not backlog"
    );

    // Issues are repo-owned: deleting the session returns its claimed issue to
    // the unclaimed backlog rather than deleting it.
    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
    let board = client
        .post(
            "/api/issues/list",
            json!({ "repo_root": repo_root, "all": true }),
        )
        .await
        .unwrap();
    let board = board.as_array().unwrap();
    assert_eq!(board.len(), 1, "manual issue survived teardown");
    assert!(
        board.iter().all(|i| i["claimed_branch"].is_null()),
        "every claim was released on teardown"
    );
    assert!(
        board.iter().any(|i| i["id"].as_i64() == Some(issue_id)),
        "the hand-created issue survived"
    );
}

/// The cross-repo issue board (`issues.board`) and issue tags: a label set
/// through the typed client surfaces on the issue's `tags`, including when its
/// free-form key contains reserved URL characters, and clearing removes it.
/// Closed issues only appear with `all: true`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_repo_board_and_issue_tags() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let repo_root = ts.cwd();

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "board me", "cwd": repo_root, "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let branch_id = ws["branch"]["id"].as_str().unwrap().to_string();

    let created = client
        .post(
            "/api/issues/create",
            json!({ "branch": branch_id, "title": "label me" }),
        )
        .await
        .unwrap();
    let issue_id = created["id"].as_i64().unwrap();
    assert!(
        created["tags"].as_array().unwrap().is_empty(),
        "a fresh issue carries no tags"
    );

    // The cross-repo board lists the open issues (tracking + the new one).
    let board = client.post("/api/issues/board", json!({})).await.unwrap();
    let board = board.as_array().unwrap();
    assert!(
        board.iter().any(|i| i["id"].as_i64() == Some(issue_id)),
        "the new issue shows on the cross-repo board"
    );

    // Set a free-form label through the registered operation. The key
    // deliberately contains path/query delimiters, which are now unremarkable:
    // the operation carries its arguments in the body, so nothing about this
    // key touches the route.
    let tag_key = "priority / now?#";
    let tagged = client
        .invoke::<tags::set::Set>(&tags::set::Input {
            id: issue_id,
            key: tag_key.to_string(),
            value: "high".to_string(),
            note: "ship first".to_string(),
            repo_root: repo_root.clone(),
        })
        .await
        .unwrap();
    assert_eq!(tagged.tags.len(), 1);
    assert_eq!(tagged.tags[0].key, tag_key);
    assert_eq!(tagged.tags[0].value, "high");
    assert_eq!(tagged.tags[0].note, "ship first");
    // Provenance is read off the credential, not the body: this client holds a
    // human token, so the tag records `manual`. A session token records
    // `agent`, and no caller can claim to be either.
    assert_eq!(tagged.tags[0].set_by, "manual");

    // An empty value is rejected (clear the tag instead).
    let bad = client
        .invoke::<tags::set::Set>(&tags::set::Input {
            id: issue_id,
            key: tag_key.to_string(),
            value: String::new(),
            note: String::new(),
            repo_root: repo_root.clone(),
        })
        .await;
    assert!(bad.is_err(), "an empty issue-tag value is rejected");

    // Clearing removes the label.
    let cleared = client
        .invoke::<tags::delete::Delete>(&tags::delete::Input {
            id: issue_id,
            key: tag_key.to_string(),
            repo_root: repo_root.clone(),
        })
        .await
        .unwrap();
    assert!(cleared.tags.is_empty(), "clearing removes the label");

    // Lifecycle operations take a set of ids and apply atomically. The single
    // -id case is just the one-element set.
    let close = |ids: Vec<i64>| {
        let input = close::Input {
            ids,
            repo_root: repo_root.clone(),
        };
        async move { client.invoke::<close::Close>(&input).await }
    };
    let closed = close(vec![issue_id]).await.unwrap();
    assert_eq!(closed.issues[0].status, "closed");
    assert!(
        close(vec![issue_id]).await.is_err(),
        "closing an already-closed issue retains atomic action validation"
    );
    let reopened = client
        .invoke::<reopen::Reopen>(&reopen::Input {
            ids: vec![issue_id],
            repo_root: repo_root.clone(),
        })
        .await
        .unwrap();
    assert_eq!(reopened.issues[0].status, "open");
    close(vec![issue_id]).await.unwrap();

    // A closed issue leaves the default board but returns with all: true.
    let open_board = client.post("/api/issues/board", json!({})).await.unwrap();
    assert!(
        !open_board
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"].as_i64() == Some(issue_id)),
        "a closed issue is off the default board"
    );
    let all_board = client
        .post("/api/issues/board", json!({ "all": true }))
        .await
        .unwrap();
    assert!(
        all_board
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"].as_i64() == Some(issue_id)),
        "all: true includes the closed issue"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// The triage axis: `sessions.tags.set` on the `triage` key stamps the watch's
/// mark on the session's branch, surfaces it on the SessionView's `branch.tags`,
/// and never disturbs the agent's own `attention` tag. An invalid value is
/// rejected.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn triage_axis_marks_a_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let repo_root = ts.cwd();

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "triage me", "cwd": repo_root, "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();

    // The agent declares `blocked` about itself — its own `attention` tag.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "session": id, "key": "attention", "value": "blocked", "by": "agent" }),
        )
        .await
        .unwrap();

    // Fresh: no watch mark yet.
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert!(
        branch_tag(&view, "triage").is_none(),
        "unmarked: no triage tag yet"
    );

    // A watch stamps a mark via the triage tag.
    let marked = client
        .post(
            "/api/sessions/tags/set",
            json!({
                "session": id,
                "key": "triage",
                "value": "attention",
                "note": "idle 30m with red CI",
                "by": "status-check"
            }),
        )
        .await
        .unwrap();
    // `sessions.tags.set` answers with the branch directly (unlike
    // `sessions.get`'s `SessionView`, there is no outer `branch` wrapper), so
    // this reads `marked["tags"]` directly.
    let triage = marked["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "triage")
        .expect("the mark wrote a triage tag");
    assert_eq!(triage["value"], "attention");
    assert_eq!(triage["note"], "idle 30m with red CI");
    assert_eq!(triage["set_by"], "status-check");
    assert!(
        triage["set_at"].as_str().is_some_and(|s| !s.is_empty()),
        "a mark stamps set_at"
    );
    // The agent's own attention is untouched — two actors, two axes.
    let attention = marked["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "attention")
        .map(|t| t["value"].as_str().unwrap_or(""))
        .unwrap_or("");
    assert_eq!(
        attention, "blocked",
        "triage must not stomp the agent's self-report"
    );

    // An invalid value is rejected.
    let bad = client
        .post(
            "/api/sessions/tags/set",
            json!({ "session": id, "key": "triage", "value": "bogus" }),
        )
        .await;
    assert!(bad.is_err(), "invalid triage value should be rejected");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// A watch replaces its complete authored tag set in one request. The
/// replacement clears dropped watch marks and an exact lifecycle mark, while
/// preserving foreign tags — including a key another actor took over after the
/// watch's snapshot went stale.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_tags_replace_one_authors_set_atomically() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let session = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "reconcile labels", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();

    for (key, value, by) in [
        ("stuck", "blocked", "status-check"),
        ("owner", "alice", "manual"),
        ("idle", "idle", "agent"),
    ] {
        client
            .post(
                "/api/sessions/tags/set",
                json!({ "session": id, "key": key, "value": value, "by": by }),
            )
            .await
            .unwrap();
    }

    let replaced = client
        .post(
            "/api/sessions/tags/replace",
            json!({
                "session": id,
                "by": "status-check",
                "tags": [
                    { "key": "review", "value": "attention", "note": "ready" }
                ],
                "clear": [{ "key": "idle", "value": "idle" }]
            }),
        )
        .await
        .unwrap();
    assert!(branch_tag(&replaced, "stuck").is_none());
    assert!(branch_tag(&replaced, "idle").is_none());
    assert_eq!(branch_tag_value(&replaced, "owner"), "alice");
    assert_eq!(branch_tag_value(&replaced, "review"), "attention");

    // The watch still holds a snapshot in which it owns `review`, but a person
    // has since replaced that key. Its next empty replacement must not delete
    // the person's newer value.
    client
        .post(
            "/api/sessions/tags/set",
            json!({ "session": id, "key": "review", "value": "keep", "by": "manual" }),
        )
        .await
        .unwrap();
    let calm = client
        .post(
            "/api/sessions/tags/replace",
            json!({ "session": id, "by": "status-check", "tags": [] }),
        )
        .await
        .unwrap();
    let review = branch_tag(&calm, "review").expect("manual takeover survives");
    assert_eq!(review["value"], "keep");
    assert_eq!(review["set_by"], "manual");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// Attaching to an existing branch reuses its worktree if one exists, creates
/// `.worktrees/<slug>` otherwise, and rejects a branch that doesn't exist.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_to_existing_branch() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let repo = ts.repo_path().to_path_buf();
    let cwd = ts.cwd();

    let branches_q = client
        .post("/api/repos/branches", json!({ "cwd": cwd }))
        .await
        .unwrap();
    let arr = branches_q.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|b| b["name"] == "main" && b["current"] == true),
        "main should be listed as current, got {arr:?}"
    );

    let valid_base = client
        .post(
            "/api/repos/revisions/validate",
            json!({ "cwd": cwd, "revision": "main" }),
        )
        .await
        .unwrap();
    assert_eq!(valid_base["valid"], true);
    assert_eq!(valid_base["message"], serde_json::Value::Null);

    let missing_base = client
        .post(
            "/api/repos/revisions/validate",
            json!({ "cwd": cwd, "revision": "agent/missing-upstream-worktree" }),
        )
        .await
        .unwrap();
    assert_eq!(missing_base["valid"], false);
    assert!(missing_base["message"]
        .as_str()
        .unwrap()
        .contains("was not found in repository"));

    // A branch with no worktree gets a fresh .worktrees/<slug>.
    sh(&repo, "git", &["branch", "feature/x", "main"]);
    let attached = client
        .post(
            "/api/sessions/launch",
            json!({
                "cwd": cwd,
                "goal": "attach to feature/x",
                "agent": "shell",
                "existing_branch": "feature/x",
            }),
        )
        .await
        .unwrap();
    assert_eq!(attached["branch"]["branch"], "feature/x");
    let attached_id = attached["id"].as_str().unwrap().to_string();
    let attached_dir = attached["work_dir"].as_str().unwrap().to_string();
    assert!(
        attached_dir.ends_with("/.worktrees/feature-x"),
        "attached worktree should live at .worktrees/feature-x, got {attached_dir}"
    );
    assert!(std::path::Path::new(&attached_dir).join(".git").exists());

    // A branch that already has a worktree reuses that exact path.
    sh(&repo, "git", &["branch", "feature/y", "main"]);
    let preexisting = repo.join("custom-worktree-y");
    sh(
        &repo,
        "git",
        &[
            "worktree",
            "add",
            preexisting.to_str().unwrap(),
            "feature/y",
        ],
    );
    let attached_y = client
        .post(
            "/api/sessions/launch",
            json!({
                "cwd": cwd,
                "goal": "attach to feature/y",
                "agent": "shell",
                "existing_branch": "feature/y",
            }),
        )
        .await
        .unwrap();
    assert_eq!(attached_y["branch"]["branch"], "feature/y");
    let dir_y = attached_y["work_dir"].as_str().unwrap().to_string();
    assert_eq!(
        std::fs::canonicalize(&dir_y).unwrap(),
        std::fs::canonicalize(&preexisting).unwrap(),
        "weaver should reuse the pre-existing worktree path"
    );

    // A non-existent branch is rejected.
    let missing = client
        .post(
            "/api/sessions/launch",
            json!({
                "cwd": cwd,
                "goal": "missing branch",
                "agent": "shell",
                "existing_branch": "no/such/branch",
            }),
        )
        .await;
    assert!(missing.is_err(), "missing branch should be rejected");

    client
        .post("/api/sessions/delete", json!({ "session": attached_id }))
        .await
        .unwrap();
    client
        .post(
            "/api/sessions/delete",
            json!({ "session": attached_y["id"].as_str().unwrap() }),
        )
        .await
        .unwrap();
}

/// The Slack reply route's destination is server-resolved. A thread the session
/// was never delivered is refused, so `thread` selects among the session's own
/// alert threads rather than granting the agent the workspace. Both refusals
/// land before any Slack call, so they hold on a server with no Slack
/// configured — which is also what a leaked `LOOM_TOKEN` would face.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slack_reply_refuses_a_thread_the_session_was_not_delivered() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "slack reply scope", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let branch_id = session["branch"]["id"].as_str().unwrap().to_string();
    let reply_url = format!("http://{}/api/branches/slack/reply", ts.addr);

    // An unrouted thread is forbidden even though it is well-formed.
    let unrouted = http
        .post(&reply_url)
        .json(&json!({
            "branch": branch_id,
            "text": "status update",
            "thread": { "channel": "C0123ABCD", "thread_ts": "1700000000.123456" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unrouted.status(), StatusCode::FORBIDDEN);

    // A channel name is not an id: rejected on shape, before any Slack call.
    let malformed = http
        .post(&reply_url)
        .json(&json!({
            "branch": branch_id,
            "text": "status update",
            "thread": { "channel": "#marin-eng", "thread_ts": "1700000000.123456" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    // Without `thread`, an unwired branch still reports that it has no thread.
    let unwired = http
        .post(&reply_url)
        .json(&json!({ "branch": branch_id, "text": "status update" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unwired.status(), StatusCode::BAD_REQUEST);

    ts.client
        .post(
            "/api/sessions/delete",
            json!({ "session": session["id"].as_str().unwrap() }),
        )
        .await
        .unwrap();
}
