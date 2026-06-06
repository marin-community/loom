//! The file viewer: the tree lists worktree files and badges changes vs base;
//! `/file` returns text (working or base ref); `/raw` returns bytes; the `base`
//! selector switches between branch-scope and uncommitted-scope diffs; and path
//! traversal is refused.

use std::path::Path;

use serde_json::json;
use serial_test::serial;

use crate::fixtures::{sh, TestServer};

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_file_raw_and_diff_baseline() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "files test",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();

    // Fresh worktree: README is listed and not yet changed.
    let tree = client
        .get(&format!("/api/sessions/{id}/tree"))
        .await
        .unwrap();
    let files: Vec<String> = tree["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        files.contains(&"README.md".to_string()),
        "tree lists README.md, got {files:?}"
    );
    assert!(
        !tree["changed"]
            .as_object()
            .unwrap()
            .contains_key("README.md"),
        "README unchanged before any edit"
    );

    let file = client
        .get(&format!("/api/sessions/{id}/file?path=README.md"))
        .await
        .unwrap();
    assert_eq!(file["content"], "hello\n");
    assert_eq!(file["binary"], false);

    // Edit a tracked file and drop a brand-new one.
    std::fs::write(Path::new(&work_dir).join("README.md"), "hello world\n").unwrap();
    std::fs::write(Path::new(&work_dir).join("new.txt"), "fresh\n").unwrap();

    let tree = client
        .get(&format!("/api/sessions/{id}/tree"))
        .await
        .unwrap();
    let changed = tree["changed"].as_object().unwrap();
    assert_eq!(changed["README.md"], "modified");
    assert_eq!(changed["new.txt"], "added", "untracked file shows as added");
    let files: Vec<String> = tree["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        files.contains(&"new.txt".to_string()),
        "untracked file listed in tree"
    );

    // base ref reads the merge-base version; working ref reads the edit.
    let base = client
        .get(&format!("/api/sessions/{id}/file?path=README.md&ref=base"))
        .await
        .unwrap();
    assert_eq!(
        base["content"], "hello\n",
        "base ref reads the merge-base version"
    );
    let work = client
        .get(&format!("/api/sessions/{id}/file?path=README.md"))
        .await
        .unwrap();
    assert_eq!(work["content"], "hello world\n");

    // Raw bytes carry the working-tree content.
    let http = reqwest::Client::new();
    let raw = http
        .get(format!(
            "{}/api/sessions/{id}/raw?path=new.txt",
            client.base()
        ))
        .send()
        .await
        .unwrap();
    assert!(raw.status().is_success());
    assert_eq!(raw.text().await.unwrap(), "fresh\n");

    // Traversal / absolute paths are refused on both reads.
    let bad = http
        .get(format!(
            "{}/api/sessions/{id}/file?path=../escape",
            client.base()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400, "file path traversal rejected");
    let bad = http
        .get(format!(
            "{}/api/sessions/{id}/raw?path=/etc/passwd",
            client.base()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400, "absolute raw path rejected");

    // Diff baseline selector: commit the current edits, then make one more
    // uncommitted change. `base=branch` (the default) shows everything the
    // branch introduced; `base=uncommitted` shows only what's not committed.
    let wt = Path::new(&work_dir);
    sh(wt, "git", &["add", "-A"]);
    sh(wt, "git", &["commit", "-q", "-m", "branch work"]);
    std::fs::write(wt.join("README.md"), "hello world again\n").unwrap();

    let branch_scope = client
        .get(&format!("/api/sessions/{id}/tree?base=branch"))
        .await
        .unwrap();
    let changed = branch_scope["changed"].as_object().unwrap();
    assert_eq!(branch_scope["base"], "branch");
    assert_eq!(changed["README.md"], "modified");
    assert_eq!(
        changed["new.txt"], "added",
        "branch scope still shows the committed addition"
    );

    let uncommitted = client
        .get(&format!("/api/sessions/{id}/tree?base=uncommitted"))
        .await
        .unwrap();
    let changed = uncommitted["changed"].as_object().unwrap();
    assert_eq!(uncommitted["base"], "uncommitted");
    assert_eq!(changed["README.md"], "modified");
    assert!(
        !changed.contains_key("new.txt"),
        "uncommitted scope hides the already-committed addition, got {changed:?}"
    );

    // The `base` ref read honours the scope too: vs HEAD the original is the
    // committed README; vs the branch fork point it's the pre-branch one.
    let head_side = client
        .get(&format!(
            "/api/sessions/{id}/file?path=README.md&ref=base&base=uncommitted"
        ))
        .await
        .unwrap();
    assert_eq!(
        head_side["content"], "hello world\n",
        "uncommitted base reads the committed (HEAD) version"
    );
    let fork_side = client
        .get(&format!(
            "/api/sessions/{id}/file?path=README.md&ref=base&base=branch"
        ))
        .await
        .unwrap();
    assert_eq!(
        fork_side["content"], "hello\n",
        "branch base reads the fork-point version"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}
