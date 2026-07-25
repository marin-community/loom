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

    // Delete removes it.
    client
        .delete(&format!("/api/sessions/{id}/scratch?name=notes.txt"))
        .await
        .unwrap();
    let after = client
        .get(&format!("/api/sessions/{id}/scratch"))
        .await
        .unwrap();
    assert_eq!(
        after.as_array().unwrap().len(),
        0,
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
