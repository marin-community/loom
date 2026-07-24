//! Session lifecycle over the REST API: create → list → recent-repos → delete,
//! plus adoption of an externally-killed session and the no-goal create path.

use std::process::Command;

use serde_json::json;
use serial_test::serial;

use loom::backend;

use crate::fixtures::{sh, TestServer};

struct EnvVarGuard {
    name: &'static str,
    value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn unset(name: &'static str) -> Self {
        let value = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.value {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

/// Creating a session provisions a worktree + terminal session and records the repo;
/// deleting it tears the terminal session down and releases the repo's active count.
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
    let session = ws["term_session"].as_str().unwrap().to_string();
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
    assert!(
        backend::has_session(&session).await,
        "terminal session missing"
    );

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

    // Deleting the session tears down the terminal session and the DB row.
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
    assert!(
        !backend::has_session(&session).await,
        "terminal session was not killed"
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

/// New-session provisioning waits at the repository boundary before it fetches,
/// selects a branch, or creates a worktree. Holding the same gate here gives the
/// route a deterministic concurrency test without racing real git subprocesses.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_creation_waits_for_the_repository_launch_gate() {
    let ts = TestServer::start().await;
    let repo = ts.repo_path().canonicalize().unwrap();
    let permit = ts.state.launch_gate.acquire(&repo).await;
    let client = weaver_api::Client::new(format!("http://{}", ts.addr));
    let cwd = ts.cwd();

    let launch = tokio::spawn(async move {
        client
            .post(
                "/api/sessions",
                json!({
                    "goal": "wait for repository",
                    "cwd": cwd,
                    "agent": "shell",
                }),
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !launch.is_finished(),
        "launch should wait while the repository permit is held"
    );
    assert!(
        !ts.repo_path().join(".worktrees").exists(),
        "git provisioning must not begin before the permit is acquired"
    );

    drop(permit);
    let session = tokio::time::timeout(std::time::Duration::from_secs(5), launch)
        .await
        .expect("launch should resume when the repository is ready")
        .unwrap()
        .unwrap();
    let id = session["id"].as_str().unwrap();
    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}

/// A real agent launch needs either the launching user's GitHub token or a
/// default GH_TOKEN source. Without one, reject before provisioning anything.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_agent_without_github_token_is_rejected_before_provisioning() {
    let _env = EnvVarGuard::unset("GH_TOKEN");
    let ts = TestServer::start().await;
    let client = &ts.client;

    let err = client
        .post(
            "/api/sessions",
            json!({
                "goal": "needs credentials",
                "cwd": ts.cwd(),
                "agent": "codex",
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("server returned 428") && err.contains("No GitHub token configured"),
        "unexpected error: {err}"
    );

    let list = client.get("/api/sessions").await.unwrap();
    assert!(
        list.as_array().unwrap().is_empty(),
        "rejected launch should not create a session row: {list}"
    );
    assert!(
        !ts.repo_path().join(".worktrees").exists(),
        "rejected launch should not create a worktree directory"
    );
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

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_validate_agent_model_effort_against_registry() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let err = client
        .patch(
            "/api/settings",
            json!({ "agent.default": "codex", "agent.model": "haiku" }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown model 'haiku' for codex"),
        "unexpected error: {err}"
    );
}

/// The fleet listing hides archived sessions by default (so the agent's `loom
/// session ls` sees only live work), includes them on `?archived=true`, and
/// narrows by substring on `?q=` — over the title, branch, and goal. A rename
/// (PATCH title) is reflected in that search.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_hides_archived_by_default_and_searches() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let alpha = client
        .post(
            "/api/sessions",
            json!({ "goal": "alpha search target", "cwd": ts.cwd(), "agent": "shell", "name": "alpha" }),
        )
        .await
        .unwrap();
    let alpha_id = alpha["id"].as_str().unwrap().to_string();

    let beta = client
        .post(
            "/api/sessions",
            json!({ "goal": "beta other work", "cwd": ts.cwd(), "agent": "shell", "name": "beta" }),
        )
        .await
        .unwrap();
    let beta_id = beta["id"].as_str().unwrap().to_string();

    // Archive beta — it leaves the active fleet.
    client
        .post(&format!("/api/sessions/{beta_id}/archive"), json!({}))
        .await
        .unwrap();

    // Default: only the live session, archived hidden.
    let list = client.get("/api/sessions").await.unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![alpha_id.as_str()], "archived hidden by default");

    // Opt in: both, beta marked archived.
    let all = client.get("/api/sessions?archived=true").await.unwrap();
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 2, "?archived=true includes the archived session");
    let beta_row = all.iter().find(|s| s["id"] == beta_id.as_str()).unwrap();
    assert_eq!(beta_row["status"], "archived");

    // Search over title / branch / goal, on the live set.
    let hit = client.get("/api/sessions?q=alpha").await.unwrap();
    assert_eq!(
        hit.as_array().unwrap().len(),
        1,
        "alpha matches its goal/name"
    );
    let miss = client.get("/api/sessions?q=nope-nothing").await.unwrap();
    assert!(miss.as_array().unwrap().is_empty(), "no match ⇒ empty");

    // An archived session is excluded from a default search, included when asked.
    let beta_hidden = client.get("/api/sessions?q=beta").await.unwrap();
    assert!(
        beta_hidden.as_array().unwrap().is_empty(),
        "archived excluded from the default search"
    );
    let beta_shown = client
        .get("/api/sessions?q=beta&archived=true")
        .await
        .unwrap();
    assert_eq!(
        beta_shown.as_array().unwrap().len(),
        1,
        "archived search opt-in finds beta"
    );

    // Renaming a session (the title PATCH the `loom session rename` CLI wraps) is
    // reflected in the search.
    client
        .patch(
            &format!("/api/sessions/{alpha_id}"),
            json!({ "title": "renamed-zeta" }),
        )
        .await
        .unwrap();
    let renamed = client.get("/api/sessions?q=zeta").await.unwrap();
    assert_eq!(
        renamed.as_array().unwrap().len(),
        1,
        "rename is reflected in search"
    );

    client
        .delete(&format!("/api/sessions/{alpha_id}"))
        .await
        .unwrap();
    client
        .delete(&format!("/api/sessions/{beta_id}"))
        .await
        .unwrap();
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

/// Adoption recovers a session whose terminal supervisor was killed out from under
/// loom: it recreates the terminal; adopting a live one is rejected.
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
    let session = ws["term_session"].as_str().unwrap().to_string();

    backend::kill_session(&session).await.unwrap();
    assert!(
        !backend::has_session(&session).await,
        "session should be gone after kill"
    );

    let adopted = client
        .post(&format!("/api/sessions/{id}/adopt"), json!({}))
        .await
        .unwrap();
    // A shell runtime is hookless, so adopt brings it straight back `running`
    // (the same status it launches with) rather than stranding it in `launching`
    // waiting for a promotion hook that never fires. A claude adopt stays
    // `launching` until its first hook.
    assert_eq!(
        adopted["status"], "running",
        "a hookless (shell) session adopts straight to running"
    );
    assert!(
        backend::has_session(&session).await,
        "adopt should recreate the terminal session"
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

/// A session records the principal that launched it (`created_by`) — attribution
/// for the shared board. The value is read from the resolving `Principal` (here
/// the loopback owner the harness authenticates as), stored on the row at create
/// time, and survives a re-list (and a get-by-id) unchanged.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_records_its_creating_principal() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Who the harness authenticates as — the resolved principal for these calls.
    // Asserting against this (rather than a hardcoded name) proves attribution is
    // read from the Principal, not pinned to one user.
    let me = client.get("/api/auth/me").await.unwrap();
    let who = me["username"].as_str().unwrap().to_string();
    assert!(!who.is_empty(), "the loopback caller resolves to a user");

    let ws = client
        .post(
            "/api/sessions",
            json!({ "goal": "attributed work", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    assert_eq!(
        ws["created_by"].as_str(),
        Some(who.as_str()),
        "the create response attributes the session to the launching principal"
    );

    // Stored, not recomputed: the attribution is still there on a plain list…
    let list = client.get("/api/sessions").await.unwrap();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .expect("session in list");
    assert_eq!(row["created_by"].as_str(), Some(who.as_str()));

    // …and on a get-by-id.
    let got = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(got["created_by"].as_str(), Some(who.as_str()));

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

/// `GET /api/sessions/{key}/url` — the link an agent hands a human. It resolves
/// by any session key (id or branch id, the `$WEAVER_BRANCH` the agent carries),
/// and honours the operator's public origin so the URL works off-box.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_url_resolves_by_key_and_honours_the_public_base() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions",
            json!({ "goal": "link me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let branch_id = ws["branch"]["id"].as_str().unwrap().to_string();

    // With no `auth.base_url`, the origin is derived from the request's Host —
    // for a loopback CLI that is the server it just talked to. Honest, and right
    // for a single-machine loom where the browser is on that same box.
    let derived = client
        .get(&format!("/api/sessions/{id}/url"))
        .await
        .unwrap();
    assert_eq!(
        derived["url"].as_str().unwrap(),
        format!("http://{}/s/{id}", ts.addr),
        "derived from the request origin"
    );

    // The branch id is a session key too — that is what `$WEAVER_BRANCH` holds,
    // so `loom session url` inside a session resolves to the same link.
    let by_branch = client
        .get(&format!("/api/sessions/{branch_id}/url"))
        .await
        .unwrap();
    assert_eq!(
        by_branch["url"], derived["url"],
        "branch id and session id name the same session"
    );

    // Once the operator declares a public origin, the URL is one an off-box
    // reader (of a PR, say) can actually open. The trailing slash is absorbed.
    client
        .patch(
            "/api/settings",
            json!({ "auth.base_url": "https://loom.example.com/" }),
        )
        .await
        .unwrap();
    let public = client
        .get(&format!("/api/sessions/{id}/url"))
        .await
        .unwrap();
    assert_eq!(
        public["url"].as_str().unwrap(),
        format!("https://loom.example.com/s/{id}"),
        "the configured public origin wins, with no doubled slash"
    );

    // And the CLI an agent actually runs: no argument, `$WEAVER_BRANCH` as loom
    // exports it into a session. It must print the bare URL and nothing else, so
    // `$(loom session url)` drops straight into a PR body.
    let out = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["session", "url"])
        .env("WEAVER_API", ts.addr.to_string())
        .env("WEAVER_BRANCH", &branch_id)
        .output()
        .expect("running loom session url");
    assert!(
        out.status.success(),
        "loom session url failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("https://loom.example.com/s/{id}\n"),
        "the bare URL, ready to interpolate"
    );

    // Outside a session, with nothing to resolve, it says so rather than
    // printing a URL for some arbitrary session.
    let out = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["session", "url"])
        .env("WEAVER_API", ts.addr.to_string())
        .env_remove("WEAVER_BRANCH")
        .output()
        .expect("running loom session url");
    assert!(
        !out.status.success(),
        "no session key ⇒ an error, not a guess"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not inside a loom session"),
        "names the cause: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

/// A hand-launched session is stamped `origin: user` / `class: interactive`, and
/// its tracking issue lives on the session row — so a plain get-by-id carries
/// it, not just the create response.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_records_origin_class_and_tracking_issue() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions",
            json!({ "goal": "stamped provenance", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    assert_eq!(ws["origin"], "user", "a plain HTTP launch is origin 'user'");
    assert_eq!(ws["class"], "interactive");
    let issue = ws["tracking_issue"]
        .as_i64()
        .expect("launch returns a tracking issue id");

    // Stored, not recomputed: the same identity comes back on a get-by-id.
    let got = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(got["origin"], "user");
    assert_eq!(got["class"], "interactive");
    assert_eq!(
        got["tracking_issue"].as_i64(),
        Some(issue),
        "the tracking issue persists past the create response"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

/// An automation-class session is machinery: absent from the default fleet
/// listing, present with `?automation=true` — symmetric with `archived`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automation_class_hidden_from_the_default_listing() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let auto = client
        .post(
            "/api/sessions",
            json!({
                "goal": "background machinery",
                "cwd": ts.cwd(),
                "agent": "shell",
                "class": "automation",
            }),
        )
        .await
        .unwrap();
    let auto_id = auto["id"].as_str().unwrap().to_string();
    assert_eq!(
        auto["class"], "automation",
        "the request's class override sticks"
    );

    let list = client.get("/api/sessions").await.unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] != auto_id.as_str()),
        "automation-class sessions are hidden by default: {list}"
    );

    let shown = client.get("/api/sessions?automation=true").await.unwrap();
    assert!(
        shown
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == auto_id.as_str()),
        "?automation=true includes the automation session: {shown}"
    );

    client
        .delete(&format!("/api/sessions/{auto_id}"))
        .await
        .unwrap();
}

/// Archiving a session frees its branch slot: a fresh session can attach to the
/// same branch via `existing_branch`, where the archived tenant used to make the
/// create 409 as busy. The branch key then resolves to the live tenant.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_frees_the_branch_for_a_new_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let first = client
        .post(
            "/api/sessions",
            json!({ "goal": "first tenant", "cwd": ts.cwd(), "agent": "shell", "name": "slot" }),
        )
        .await
        .unwrap();
    let first_id = first["id"].as_str().unwrap().to_string();
    let branch_ref = first["branch"]["branch"].as_str().unwrap().to_string();

    client
        .post(&format!("/api/sessions/{first_id}/archive"), json!({}))
        .await
        .unwrap();

    // The archived session no longer occupies the slot: a fresh session attaches
    // to the kept branch (re-provisioning its worktree).
    let second = client
        .post(
            "/api/sessions",
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
    assert_eq!(second["branch"]["branch"], branch_ref.as_str());
    assert_ne!(second_id, first_id, "a new session, not a resume");

    // The branch key resolves to the live tenant, not the archived one.
    let branch_id = second["branch"]["id"].as_str().unwrap().to_string();
    let got = client
        .get(&format!("/api/sessions/{branch_id}"))
        .await
        .unwrap();
    assert_eq!(got["id"], second_id.as_str());

    client
        .delete(&format!("/api/sessions/{second_id}"))
        .await
        .unwrap();
}

/// The issue board hides an issue only while its branch's *current* claim-holder
/// is automation-class. Archiving releases the issue to the visible backlog;
/// historical automation tenancy must not hide work that no session owns.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_hiding_follows_the_branch_current_claim_holder() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let auto = client
        .post(
            "/api/sessions",
            json!({
                "goal": "background run",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "relet",
                "class": "automation",
            }),
        )
        .await
        .unwrap();
    let auto_id = auto["id"].as_str().unwrap().to_string();
    let branch_ref = auto["branch"]["branch"].as_str().unwrap().to_string();
    let issue = auto["tracking_issue"].as_i64().unwrap();

    let on_board = |board: serde_json::Value| {
        board
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"].as_i64() == Some(issue))
    };

    let board = client.get("/api/issues").await.unwrap();
    assert!(
        !on_board(board),
        "an issue claimed by a live automation session is hidden by default"
    );

    client
        .post(&format!("/api/sessions/{auto_id}/archive"), json!({}))
        .await
        .unwrap();
    let board = client.get("/api/issues").await.unwrap();
    assert!(
        on_board(board),
        "an archived automation run releases its issue to the visible backlog"
    );

    // A person picks the branch back up: the freed slot is re-let interactively.
    let human = client
        .post(
            "/api/sessions",
            json!({
                "goal": "picked up by hand",
                "cwd": ts.cwd(),
                "agent": "shell",
                "existing_branch": branch_ref,
            }),
        )
        .await
        .unwrap();
    let human_id = human["id"].as_str().unwrap().to_string();

    let board = client.get("/api/issues").await.unwrap();
    assert!(
        on_board(board),
        "an interactive re-let surfaces the branch's issues on the default board"
    );

    client
        .delete(&format!("/api/sessions/{human_id}"))
        .await
        .unwrap();
}
