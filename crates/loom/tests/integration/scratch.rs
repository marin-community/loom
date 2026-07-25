//! Scratch files: an upload lands at `scratch/<name>` in the worktree, is
//! listed, and can be deleted. Path-traversal names are rejected.

use std::path::Path;

use base64::Engine as _;
use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scratch_upload_list_and_delete() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let limits = client.get("/api/scratch/limits").await.unwrap();
    assert_eq!(limits["max_files"], 20);
    assert_eq!(limits["max_file_bytes"], 25 * 1024 * 1024);
    assert_eq!(limits["max_total_bytes"], 50 * 1024 * 1024);

    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "scratch test",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();

    let scratch = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    assert_eq!(scratch.as_array().unwrap().len(), 0, "scratch starts empty");

    let http = reqwest::Client::new();
    let upload_url = format!("{}/api/sessions/{id}/scratch?name=notes.txt", client.base());
    let resp = http
        .post(&upload_url)
        .body("hello agent")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "upload should succeed");

    // It physically exists under the worktree's scratch/ directory.
    let dropped = std::fs::read_to_string(Path::new(&work_dir).join("scratch/notes.txt")).unwrap();
    assert_eq!(dropped, "hello agent");

    let listed = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    let arr = listed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "notes.txt");
    assert_eq!(arr[0]["bytes"], 11);

    // Traversal attempts are refused.
    let bad = http
        .post(format!(
            "{}/api/sessions/{id}/scratch?name=../escape.txt",
            client.base()
        ))
        .body("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400, "path traversal rejected");
    let housekeeping = reqwest::Client::new()
        .post(format!(
            "{}/api/sessions/{id}/scratch?name=.gitignore",
            client.base()
        ))
        .body("overwrite")
        .send()
        .await
        .unwrap();
    assert_eq!(housekeeping.status().as_u16(), 400);

    let dotfile = reqwest::Client::new()
        .post(format!(
            "{}/api/sessions/{id}/scratch?name=.env.example",
            client.base()
        ))
        .body("SAFE=value")
        .send()
        .await
        .unwrap();
    assert!(dotfile.status().is_success());
    let listed = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == ".env.example"));

    // Delete removes it.
    client
        .delete(&format!("/api/sessions/{id}/scratch?name=notes.txt"))
        .await
        .unwrap();
    let after = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["name"], ".env.example");
    client
        .delete(&format!("/api/sessions/{id}/scratch?name=.env.example"))
        .await
        .unwrap();
    let after = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    assert!(
        after.as_array().unwrap().is_empty(),
        "scratch empty after delete"
    );

    // Validation and write are one per-session critical section. Start with 19
    // files, then release two different uploads together: exactly one may claim
    // the twentieth slot.
    let scratch_dir = Path::new(&work_dir).join("scratch");
    for index in 0..19 {
        std::fs::write(scratch_dir.join(format!("seed-{index:02}.txt")), "x").unwrap();
    }
    let permit = ts.state.launch_gate.acquire_scratch(&id).await;
    let first_http = http.clone();
    let first_url = format!(
        "{}/api/sessions/{id}/scratch?name=concurrent-a.txt",
        client.base()
    );
    let first =
        tokio::spawn(async move { first_http.post(first_url).body("a").send().await.unwrap() });
    let second_url = format!(
        "{}/api/sessions/{id}/scratch?name=concurrent-b.txt",
        client.base()
    );
    let second = tokio::spawn(async move { http.post(second_url).body("b").send().await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !first.is_finished(),
        "first upload waits for Scratch mutation"
    );
    assert!(
        !second.is_finished(),
        "second upload waits for the same Scratch mutation"
    );
    drop(permit);

    let first = tokio::time::timeout(std::time::Duration::from_secs(5), first)
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap();
    let statuses = [first.status(), second.status()];
    assert_eq!(
        statuses.iter().filter(|status| status.is_success()).count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| status.as_u16() == 400)
            .count(),
        1
    );
    let listed = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 20);

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_initial_scratch_has_no_provisioning_side_effects() {
    let ts = TestServer::start().await;
    let backlog = ts
        .client
        .post(
            "/api/repos/issues",
            json!({
                "repo_root": ts.cwd(),
                "title": "must remain unclaimed",
                "body": ""
            }),
        )
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/sessions", ts.addr))
        .json(&json!({
            "cwd": ts.cwd(),
            "goal": "must remain side-effect free",
            "name": "invalid-scratch",
            "agent": "shell",
            "claim_issue": backlog["id"],
            "scratch": [{
                "name": ".gitignore",
                "content_base64": "bm90IHRoZSBndWFyZA=="
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    assert!(!loom::git::branch_exists(ts.repo_path(), "weaver/invalid-scratch").await);
    assert!(!ts.repo_path().join(".worktrees/invalid-scratch").exists());
    let branches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM branches")
        .fetch_one(&ts.state.db)
        .await
        .unwrap();
    let issues: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issues")
        .fetch_one(&ts.state.db)
        .await
        .unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&ts.state.db)
        .await
        .unwrap();
    assert_eq!((branches, issues, sessions), (0, 1, 0));
    let backlog = ts
        .client
        .get(&format!("/api/issues/{}", backlog["id"].as_i64().unwrap()))
        .await
        .unwrap();
    assert!(
        backlog["claimed_branch"].is_null(),
        "attachment validation runs before claiming existing work"
    );
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_advertised_scratch_total_fits_the_create_transport_envelope() {
    let ts = TestServer::start().await;
    let bytes = vec![b'x'; 25 * 1024 * 1024];
    let content = base64::engine::general_purpose::STANDARD.encode(bytes);
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/sessions", ts.addr))
        .json(&json!({
            "cwd": ts.cwd(),
            "goal": "exact scratch boundary",
            "agent": "shell",
            "scratch": [
                { "name": "first.bin", "content_base64": content },
                { "name": "second.bin", "content_base64": content }
            ]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(
        status.is_success(),
        "50 MiB decoded request was rejected at transport: {body}"
    );
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    ts.client
        .delete(&format!(
            "/api/sessions/{}",
            created["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
}
