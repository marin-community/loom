//! The managed repo store over the REST API: register a repo into the allowlist,
//! then launch a session with `{repo: "owner/name"}` so loom clones it into the
//! managed store and forks the worktree from the clone — no `cwd`. Plus the
//! security gates: traversal identifiers and non-allowlisted repos are rejected.
//!
//! The clone source is a *local bare repo* (registered via a `file://` URL), so
//! these tests never touch the network.

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

/// Register a repo, then create a session against it by slug (no `cwd`): loom
/// clones the registered remote into the managed store and forks the worktree
/// from that managed checkout.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_then_launch_clones_into_managed_store() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let remotes = tempfile::tempdir().unwrap();
    let remote_url = make_bare_remote(remotes.path());

    // Register the repo via the URL form — slug derives to `acme/widgets`.
    let reg = client
        .post("/api/repos", json!({ "repo": remote_url }))
        .await
        .unwrap();
    assert_eq!(reg["slug"], "acme/widgets");
    assert_eq!(reg["remote_url"], remote_url);

    // It shows up in the allowlist listing.
    let list = client.get("/api/repos").await.unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["slug"], "acme/widgets");

    // Launch by slug, with no cwd: loom clones the repo and uses it as the root.
    let ws = client
        .post(
            "/api/sessions",
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
            "/api/sessions",
            json!({ "goal": "managed clone two", "repo": "acme/widgets", "agent": "shell" }),
        )
        .await
        .unwrap();
    let id2 = ws2["id"].as_str().unwrap().to_string();

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
    client
        .delete(&format!("/api/sessions/{id2}"))
        .await
        .unwrap();
}

/// Security: a traversal identifier and a non-allowlisted repo are both rejected
/// with a 400 before any clone is attempted.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_rejects_traversal_and_unregistered_repos() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Traversal / malformed identifiers — rejected by the strict slug parse.
    for bad in ["../etc", "/etc/passwd", "a/b/c", "owner/.."] {
        let err = client
            .post("/api/sessions", json!({ "repo": bad, "agent": "shell" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("400"),
            "traversal id {bad:?} should be a 400, got {err}"
        );
    }

    // A clean slug that is not registered — refused by the allowlist boundary.
    let err = client
        .post(
            "/api/sessions",
            json!({ "repo": "ghost/repo", "agent": "shell" }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("400") && err.contains("not registered"),
        "unregistered repo should be a 400, got {err}"
    );

    // Registration itself rejects a traversal identifier.
    let err = client
        .post("/api/repos", json!({ "repo": "../escape" }))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("400"),
        "register traversal should 400, got {err}"
    );
}

/// The trusted-owner allowlist over the REST API: add, list, remove, and reject a
/// malformed login. The deploy owner is seeded, so a freshly-added owner appears
/// alongside it.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_owners_crud() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Add an owner.
    let added = client
        .post("/api/github/owners", json!({ "login": "acme" }))
        .await
        .unwrap();
    assert_eq!(added["login"], "acme");

    // It appears in the list (alongside the seeded deploy owner).
    let list = client.get("/api/github/owners").await.unwrap();
    let logins: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["login"].as_str().unwrap())
        .collect();
    assert!(
        logins.contains(&"acme"),
        "acme should be listed, got {logins:?}"
    );

    // A malformed login is a 400.
    let err = client
        .post("/api/github/owners", json!({ "login": "bad owner" }))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("400"), "malformed login should 400, got {err}");

    // Remove it; a second remove is a 404.
    client.delete("/api/github/owners/acme").await.unwrap();
    let err = client
        .delete("/api/github/owners/acme")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("404"),
        "removing a missing owner should 404, got {err}"
    );
}
