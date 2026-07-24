//! Branches and their issues: the tracking issue a launch opens, branch-claimed
//! issues vs the repo-wide board, claim release on teardown, and attaching a
//! session to a pre-existing git branch (with or without an existing worktree).

use serde_json::json;
use serial_test::serial;

use crate::fixtures::{branch_tag, branch_tag_value, sh, TestServer};

/// A launch opens a self-sourced tracking issue; hand-created issues are claimed
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
            "/api/sessions",
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
    let branches = client.get("/api/branches").await.unwrap();
    let arr = branches.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one branch tracked");
    assert_eq!(arr[0]["branch"], "weaver/integration-test-goal");
    // The launch opened one tracking issue, claimed by the new branch. With no
    // parent agent it is self-sourced (source_branch == claimed_branch).
    assert_eq!(
        arr[0]["open_issue_count"], 1,
        "launch opens a tracking issue"
    );
    let tracking = client
        .get(&format!("/api/branches/{branch_id}/issues"))
        .await
        .unwrap();
    let tracking = tracking.as_array().unwrap();
    assert_eq!(tracking.len(), 1, "exactly the tracking issue");
    assert_eq!(
        tracking[0]["claimed_branch"],
        "weaver/integration-test-goal"
    );
    assert_eq!(
        tracking[0]["source_branch"], "weaver/integration-test-goal",
        "self-sourced when no parent launched it"
    );

    // Branch issues are claimed by the branch; the repo-wide board lives at
    // /api/repos/issues.
    let created = client
        .post(
            &format!("/api/branches/{branch_id}/issues"),
            json!({ "title": "fix it", "body": "details" }),
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
        .get(&format!("/api/branches/{branch_id}/issues"))
        .await
        .unwrap();
    assert_eq!(
        listed.as_array().unwrap().len(),
        2,
        "the tracking issue plus the hand-created one"
    );
    let branch_view = client
        .get(&format!("/api/branches/{branch_id}"))
        .await
        .unwrap();
    assert_eq!(branch_view["open_issue_count"], 2);
    // The repo board sees both claimed issues; the unclaimed backlog does not.
    let board = client
        .get(&format!("/api/repos/issues?repo_root={repo_root}"))
        .await
        .unwrap();
    assert_eq!(board.as_array().unwrap().len(), 2);
    let backlog = client
        .get(&format!(
            "/api/repos/issues?repo_root={repo_root}&scope=backlog"
        ))
        .await
        .unwrap();
    assert_eq!(
        backlog.as_array().unwrap().len(),
        0,
        "issue is claimed, not backlog"
    );

    // Issues are repo-owned: deleting the session returns its claimed issues to
    // the unclaimed backlog rather than deleting them. The tracking issue and
    // the hand-created "fix it" both survive, every claim released.
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
    let board = client
        .get(&format!("/api/repos/issues?repo_root={repo_root}&all=true"))
        .await
        .unwrap();
    let board = board.as_array().unwrap();
    assert_eq!(board.len(), 2, "tracking + manual issues survived teardown");
    assert!(
        board.iter().all(|i| i["claimed_branch"].is_null()),
        "every claim was released on teardown"
    );
    assert!(
        board.iter().any(|i| i["id"].as_i64() == Some(issue_id)),
        "the hand-created issue survived"
    );
}

/// The cross-repo issue board (`GET /api/issues`) and issue tags: a label set
/// via `PUT /api/issues/{id}/tags/{key}` surfaces on the issue's `tags`, and
/// `DELETE` clears it. Closed issues only appear with `?all=true`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_repo_board_and_issue_tags() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let repo_root = ts.cwd();

    let ws = client
        .post(
            "/api/sessions",
            json!({ "goal": "board me", "cwd": repo_root, "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let branch_id = ws["branch"]["id"].as_str().unwrap().to_string();

    let created = client
        .post(
            &format!("/api/branches/{branch_id}/issues"),
            json!({ "title": "label me" }),
        )
        .await
        .unwrap();
    let issue_id = created["id"].as_i64().unwrap();
    assert!(
        created["tags"].as_array().unwrap().is_empty(),
        "a fresh issue carries no tags"
    );

    // The cross-repo board lists the open issues (tracking + the new one).
    let board = client.get("/api/issues").await.unwrap();
    let board = board.as_array().unwrap();
    assert!(
        board.iter().any(|i| i["id"].as_i64() == Some(issue_id)),
        "the new issue shows on the cross-repo board"
    );

    // Set a free-form label; it surfaces on the issue's tags with attribution.
    let tagged = client
        .put(
            &format!("/api/issues/{issue_id}/tags/priority"),
            json!({ "value": "high", "note": "ship first", "by": "agent" }),
        )
        .await
        .unwrap();
    let tags = tagged["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0]["key"], "priority");
    assert_eq!(tags[0]["value"], "high");
    assert_eq!(tags[0]["note"], "ship first");
    assert_eq!(tags[0]["set_by"], "agent");

    // An empty value is rejected (clear the tag instead).
    let bad = client
        .put(
            &format!("/api/issues/{issue_id}/tags/priority"),
            json!({ "value": "" }),
        )
        .await;
    assert!(bad.is_err(), "an empty issue-tag value is rejected");

    // Clearing removes the label.
    let cleared = client
        .delete(&format!("/api/issues/{issue_id}/tags/priority"))
        .await
        .unwrap();
    assert!(
        cleared["tags"].as_array().unwrap().is_empty(),
        "clearing removes the label"
    );

    // Close the issue: it leaves the default board but returns with ?all=true.
    client
        .patch(
            &format!("/api/issues/{issue_id}"),
            json!({ "status": "closed" }),
        )
        .await
        .unwrap();
    let open_board = client.get("/api/issues").await.unwrap();
    assert!(
        !open_board
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"].as_i64() == Some(issue_id)),
        "a closed issue is off the default board"
    );
    let all_board = client.get("/api/issues?all=true").await.unwrap();
    assert!(
        all_board
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"].as_i64() == Some(issue_id)),
        "?all=true includes the closed issue"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

/// The triage axis: `PUT /api/sessions/{id}/tags/triage` stamps the watch's
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
            "/api/sessions",
            json!({ "goal": "triage me", "cwd": repo_root, "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();

    // The agent declares `blocked` about itself — its own `attention` tag.
    client
        .put(
            &format!("/api/sessions/{id}/tags/attention"),
            json!({ "value": "blocked", "by": "agent" }),
        )
        .await
        .unwrap();

    // Fresh: no watch mark yet.
    let view = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert!(
        branch_tag(&view, "triage").is_none(),
        "unmarked: no triage tag yet"
    );

    // A watch stamps a mark via the triage tag.
    let marked = client
        .put(
            &format!("/api/sessions/{id}/tags/triage"),
            json!({ "value": "attention", "note": "idle 30m with red CI", "by": "status-check" }),
        )
        .await
        .unwrap();
    let triage = branch_tag(&marked, "triage").expect("the mark wrote a triage tag");
    assert_eq!(triage["value"], "attention");
    assert_eq!(triage["note"], "idle 30m with red CI");
    assert_eq!(triage["set_by"], "status-check");
    assert!(
        triage["set_at"].as_str().is_some_and(|s| !s.is_empty()),
        "a mark stamps set_at"
    );
    // The agent's own attention is untouched — two actors, two axes.
    assert_eq!(
        branch_tag_value(&marked, "attention"),
        "blocked",
        "triage must not stomp the agent's self-report"
    );

    // An invalid value is rejected.
    let bad = client
        .put(
            &format!("/api/sessions/{id}/tags/triage"),
            json!({ "value": "bogus" }),
        )
        .await;
    assert!(bad.is_err(), "invalid triage value should be rejected");

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
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
            "/api/sessions",
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
            .put(
                &format!("/api/sessions/{id}/tags/{key}"),
                json!({ "value": value, "by": by }),
            )
            .await
            .unwrap();
    }

    let replaced = client
        .put(
            &format!("/api/sessions/{id}/tags"),
            json!({
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
        .put(
            &format!("/api/sessions/{id}/tags/review"),
            json!({ "value": "keep", "by": "manual" }),
        )
        .await
        .unwrap();
    let calm = client
        .put(
            &format!("/api/sessions/{id}/tags"),
            json!({ "by": "status-check", "tags": [] }),
        )
        .await
        .unwrap();
    let review = branch_tag(&calm, "review").expect("manual takeover survives");
    assert_eq!(review["value"], "keep");
    assert_eq!(review["set_by"], "manual");

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
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
        .get(&format!("/api/repos/branches?cwd={cwd}"))
        .await
        .unwrap();
    let arr = branches_q.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|b| b["name"] == "main" && b["current"] == true),
        "main should be listed as current, got {arr:?}"
    );

    // A branch with no worktree gets a fresh .worktrees/<slug>.
    sh(&repo, "git", &["branch", "feature/x", "main"]);
    let attached = client
        .post(
            "/api/sessions",
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
            "/api/sessions",
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
            "/api/sessions",
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
        .delete(&format!("/api/sessions/{attached_id}"))
        .await
        .unwrap();
    client
        .delete(&format!(
            "/api/sessions/{}",
            attached_y["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
}
