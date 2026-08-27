//! The managed repo store over the REST API: launch a session with
//! `{repo: "owner/name"}` and loom clones the repo into the managed store and
//! forks the worktree from that clone — no `cwd`. It works whether the repo was
//! registered up front (`repos.register`) or is being named for the first time,
//! so `loom launch --repo owner/name` can reach a repo this machine has never
//! checked out. Plus the security gate that survives: traversal identifiers
//! are rejected.
//!
//! The clone source is a *local bare repo* (named by a `file://` URL), so these
//! tests never touch the network.

use serde_json::json;
use serial_test::serial;

use crate::fixtures::{sh, TestServer};

/// Lay out a bare repo at `<root>/acme/widgets` (so its trailing path is the
/// `acme/widgets` slug) with a single commit on `main`, and return its
/// `file://` clone URL.
fn make_bare_remote(root: &std::path::Path) -> String {
    // A throwaway working repo with one commit.
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    sh(&work, "git", &["init", "-q", "-b", "main"]);
    sh(&work, "git", &["config", "user.email", "t@t.test"]);
    sh(&work, "git", &["config", "user.name", "Test"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    sh(&work, "git", &["add", "."]);
    sh(&work, "git", &["commit", "-q", "-m", "init"]);

    // Bare-clone it to <root>/acme/widgets — the path whose tail is the slug.
    let bare = root.join("acme").join("widgets");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    sh(
        &work,
        "git",
        &[
            "clone",
            "--bare",
            "-q",
            &work.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    format!("file://{}", bare.display())
}

async fn allow_app_access(ts: &TestServer) {
    let mut profile = loom::profile::get(&ts.state.db, loom::profile::DEFAULT_PROFILE)
        .await
        .unwrap()
        .unwrap()
        .as_input()
        .unwrap();
    profile.github_repositories = vec!["acme/widgets".to_string()];
    loom::profile::upsert(&ts.state.db, &profile).await.unwrap();
}

/// Register a repo, then create a session against it by slug (no `cwd`): loom
/// clones the registered remote into the managed store and forks the worktree
/// from that managed checkout.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_then_launch_clones_into_managed_store() {
    let ts = TestServer::start_with_app().await;
    let client = &ts.client;
    allow_app_access(&ts).await;

    let remotes = tempfile::tempdir().unwrap();
    let remote_url = make_bare_remote(remotes.path());

    // Register the repo via the URL form — slug derives to `acme/widgets`.
    let reg = client
        .post("/api/repos/register", json!({ "repo": remote_url }))
        .await
        .unwrap();
    assert_eq!(reg["slug"], "acme/widgets");
    assert_eq!(reg["remote_url"], remote_url);

    // It shows up in the allowlist listing.
    let list = client.post("/api/repos/list", json!({})).await.unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["slug"], "acme/widgets");

    // Launch by slug, with no cwd: loom clones the repo and uses it as the root.
    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "managed clone", "repo": "acme/widgets", "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();
    let repo_root = ws["branch"]["repo_root"].as_str().unwrap().to_string();

    // The repo root is the managed clone path; the worktree lives under it.
    let managed = loom::repo::repos_dir().join("acme").join("widgets");
    assert!(
        std::path::Path::new(&work_dir).join(".git").exists(),
        "worktree was not created in the managed clone"
    );
    assert!(
        work_dir.contains("/acme/widgets/.worktrees/"),
        "worktree should live in the managed repo, got {work_dir}"
    );
    assert!(
        std::path::Path::new(&repo_root).ends_with("acme/widgets")
            || repo_root == managed.canonicalize().unwrap().to_string_lossy(),
        "repo_root should be the managed clone, got {repo_root}"
    );
    assert!(
        managed.join(".git").exists(),
        "managed clone exists on disk"
    );

    // A second launch against the same slug reuses the clone (idempotent fetch).
    let ws2 = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "managed clone two", "repo": "acme/widgets", "agent": "shell" }),
        )
        .await
        .unwrap();
    let id2 = ws2["id"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
    client
        .post("/api/sessions/delete", json!({ "session": id2 }))
        .await
        .unwrap();
}

/// Launching into a repo loom has never seen needs no separate "add the repo"
/// step: naming it on an authenticated create registers it and clones it. This is
/// what `loom launch --repo owner/name` and the new-session drawer both rely on.
///
/// (The repo is named by its `file://` URL rather than a bare slug, so the clone
/// stays local — a bare slug would resolve to its canonical github.com remote.)
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launching_into_an_unregistered_repo_registers_and_clones_it() {
    let ts = TestServer::start_with_app().await;
    let client = &ts.client;
    allow_app_access(&ts).await;

    let remotes = tempfile::tempdir().unwrap();
    let remote_url = make_bare_remote(remotes.path());

    // Nothing is registered yet.
    assert!(client
        .post("/api/repos/list", json!({}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());

    // Launch straight at it — no repos.register first.
    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "first sight", "repo": remote_url, "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();

    // It was cloned into the managed store and forked from there.
    assert!(
        work_dir.contains("/acme/widgets/.worktrees/"),
        "worktree should live in the managed clone, got {work_dir}"
    );
    assert!(loom::repo::repos_dir()
        .join("acme")
        .join("widgets")
        .join(".git")
        .exists());

    // And the launch registered it, so it is a known repo from now on.
    let list = client.post("/api/repos/list", json!({})).await.unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["slug"], "acme/widgets");
    assert_eq!(list[0]["remote_url"], remote_url);

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// Security: a traversal identifier is rejected with a 400 before any clone is
/// attempted, on both the create and the register path.
///
/// This does not assert an unregistered repo: naming a repo on an
/// authenticated create is the grant that registers it (see the test above) — the
/// `repos` allowlist gates the *unauthenticated* GitHub webhook, which resolves
/// its own clone through `repo::resolve_clone` before it reaches the shared
/// create path.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_rejects_traversal_identifiers() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Traversal / malformed identifiers — rejected by the strict slug parse.
    for bad in ["../etc", "/etc/passwd", "a/b/c", "owner/.."] {
        let err = client
            .post(
                "/api/sessions/launch",
                json!({ "repo": bad, "agent": "shell" }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("400"),
            "traversal id {bad:?} should be a 400, got {err}"
        );
    }

    // Registration itself rejects a traversal identifier.
    let err = client
        .post("/api/repos/register", json!({ "repo": "../escape" }))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("400"),
        "register traversal should 400, got {err}"
    );
}
