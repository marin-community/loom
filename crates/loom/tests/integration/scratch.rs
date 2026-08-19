//! Scratch files: an upload lands at `scratch/<name>` in the worktree, is
//! listed, and can be deleted. Path-traversal names are rejected.

use std::path::Path;

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scratch_upload_list_and_delete() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let limits = client
        .post("/api/sessions/scratch/limits", json!({}))
        .await
        .unwrap();
    assert_eq!(limits["max_files"], 20);
    assert_eq!(limits["max_file_bytes"], 25 * 1024 * 1024);
    assert_eq!(limits["max_total_bytes"], 50 * 1024 * 1024);
    assert_eq!(limits["max_name_bytes"], 240);

    let ws = client
        .post(
            "/api/sessions/launch",
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
        .post("/api/sessions/scratch/list", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(scratch.as_array().unwrap().len(), 0, "scratch starts empty");

    let http = reqwest::Client::new();
    let upload_url = format!(
        "{}/api/sessions/scratch/write?session={id}&name=notes.txt",
        client.base()
    );
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
        .post("/api/sessions/scratch/list", json!({ "session": id }))
        .await
        .unwrap();
    let arr = listed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "notes.txt");
    assert_eq!(arr[0]["bytes"], 11);

    // Traversal attempts are refused.
    let bad = http
        .post(format!(
            "{}/api/sessions/scratch/write?session={id}&name=../escape.txt",
            client.base()
        ))
        .body("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400, "path traversal rejected");
    let housekeeping = reqwest::Client::new()
        .post(format!(
            "{}/api/sessions/scratch/write?session={id}&name=.gitignore",
            client.base()
        ))
        .body("overwrite")
        .send()
        .await
        .unwrap();
    assert_eq!(housekeeping.status().as_u16(), 400);
    for invalid_name in [
        "nul\0name.txt".to_string(),
        format!("{}.txt", "x".repeat(241)),
    ] {
        let invalid = reqwest::Client::new()
            .post(format!("{}/api/sessions/scratch/write", client.base()))
            .query(&[("session", id.as_str()), ("name", invalid_name.as_str())])
            .body("nope")
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    let dotfile = reqwest::Client::new()
        .post(format!(
            "{}/api/sessions/scratch/write?session={id}&name=.env.example",
            client.base()
        ))
        .body("SAFE=value")
        .send()
        .await
        .unwrap();
    assert!(dotfile.status().is_success());
    let listed = client
        .post("/api/sessions/scratch/list", json!({ "session": id }))
        .await
        .unwrap();
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == ".env.example"));

    // Delete removes it.
    client
        .post(
            "/api/sessions/scratch/delete",
            json!({ "session": id, "name": "notes.txt" }),
        )
        .await
        .unwrap();
    let after = client
        .post("/api/sessions/scratch/list", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["name"], ".env.example");
    client
        .post(
            "/api/sessions/scratch/delete",
            json!({ "session": id, "name": ".env.example" }),
        )
        .await
        .unwrap();
    let after = client
        .post("/api/sessions/scratch/list", json!({ "session": id }))
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
    let permit = ts
        .state
        .launch_gate
        .acquire_scratch(Path::new(&work_dir))
        .await;
    let first_http = http.clone();
    let first_url = format!(
        "{}/api/sessions/scratch/write?session={id}&name=concurrent-a.txt",
        client.base()
    );
    let first =
        tokio::spawn(async move { first_http.post(first_url).body("a").send().await.unwrap() });
    let second_url = format!(
        "{}/api/sessions/scratch/write?session={id}&name=concurrent-b.txt",
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
        .post("/api/sessions/scratch/list", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 20);

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reused_worktree_launch_and_live_upload_share_one_inventory_boundary() {
    let ts = TestServer::start().await;
    let original = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "cwd": ts.cwd(),
                "goal": "own main before replacement",
                "agent": "shell",
                "existing_branch": "main"
            }),
        )
        .await
        .unwrap();
    let original_id = original["id"].as_str().unwrap().to_string();
    ts.client
        .post(
            "/api/sessions/update",
            json!({ "session": original_id, "status": "error" }),
        )
        .await
        .unwrap();
    let work_dir = original["work_dir"].as_str().unwrap().to_string();
    let scratch = Path::new(&work_dir).join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join(".gitignore"), "*\n").unwrap();
    for index in 0..19 {
        std::fs::write(scratch.join(format!("race-{index:02}.txt")), "x").unwrap();
    }

    let permit = ts
        .state
        .launch_gate
        .acquire_scratch(Path::new(&work_dir))
        .await;
    let launch_url = format!("http://{}/api/sessions/launch", ts.addr);
    let launch_cwd = ts.cwd();
    let launch = tokio::spawn(async move {
        reqwest::Client::new()
            .post(launch_url)
            .json(&json!({
                "cwd": launch_cwd,
                "goal": "replacement claims twentieth slot",
                "agent": "shell",
                "existing_branch": "main",
                "scratch": [{
                    "name": "from-launch.txt",
                    "content_base64": "eA=="
                }]
            }))
            .send()
            .await
            .unwrap()
    });
    let upload_url = format!(
        "http://{}/api/sessions/scratch/write?session={original_id}&name=from-upload.txt",
        ts.addr
    );
    let upload = tokio::spawn(async move {
        reqwest::Client::new()
            .post(upload_url)
            .body("x")
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!launch.is_finished());
    assert!(!upload.is_finished());
    drop(permit);

    let launch = tokio::time::timeout(std::time::Duration::from_secs(15), launch)
        .await
        .unwrap()
        .unwrap();
    let upload = tokio::time::timeout(std::time::Duration::from_secs(15), upload)
        .await
        .unwrap()
        .unwrap();
    let statuses = [launch.status(), upload.status()];
    assert_eq!(
        statuses.iter().filter(|status| status.is_success()).count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::BAD_REQUEST)
            .count(),
        1
    );
    let files = std::fs::read_dir(&scratch)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != ".gitignore")
        .count();
    assert_eq!(files, 20);
}
