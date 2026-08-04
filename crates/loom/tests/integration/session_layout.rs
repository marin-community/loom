use std::process::Command;

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layout_http_session_view_conflict_and_cli_share_one_contract() {
    let ts = TestServer::start().await;
    let created = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "representative layout wiring",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "layout-wiring"
            }),
        )
        .await
        .unwrap();
    let session_id = created["id"].as_str().unwrap();
    assert_eq!(created["placement"]["group_id"], "group-user-inbox");

    let seeded = ts.client.get("/api/session-layout").await.unwrap();
    assert_eq!(seeded["spaces"].as_array().unwrap().len(), 4);
    assert_eq!(seeded["spaces"][0]["name"], "User");
    let slack = seeded["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|space| space["id"] == "space-slack")
        .unwrap();
    assert_eq!(slack["name"], "Slack");
    assert_eq!(slack["groups"][0]["id"], "group-slack-inbox");
    assert_eq!(slack["groups"][0]["name"], "Inbox");
    let seeded_revision = seeded["revision"].as_i64().unwrap();

    let with_group = ts
        .client
        .post(
            "/api/session-layout/groups",
            json!({
                "space_id": "space-user",
                "name": "Focused",
                "expected_revision": seeded_revision
            }),
        )
        .await
        .unwrap();
    let focused_id = with_group["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|space| space["groups"].as_array().unwrap())
        .find(|group| group["name"] == "Focused")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let group_revision = with_group["revision"].as_i64().unwrap();

    let moved = ts
        .client
        .post(
            "/api/session-layout/moves",
            json!({
                "session_ids": [session_id],
                "destination_group_id": focused_id,
                "expected_revision": group_revision
            }),
        )
        .await
        .unwrap();
    let move_revision = moved["revision"].as_i64().unwrap();
    assert_eq!(
        moved["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|space| space["groups"].as_array().unwrap())
            .find(|group| group["id"] == focused_id)
            .unwrap()["session_ids"],
        json!([session_id])
    );

    let session = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(session["placement"]["group_id"], focused_id);
    assert_eq!(session["placement"]["group_name"], "Focused");

    let stale = reqwest::Client::new()
        .post(format!("http://{}/api/session-layout/moves", ts.addr))
        .json(&json!({
            "session_ids": [session_id],
            "destination_group_id": "group-user-inbox",
            "expected_revision": group_revision
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let stale_body: serde_json::Value = stale.json().await.unwrap();
    assert_eq!(stale_body["layout"]["revision"], move_revision);
    assert_eq!(
        stale_body["layout"]["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|space| space["groups"].as_array().unwrap())
            .find(|group| group["id"] == focused_id)
            .unwrap()["session_ids"],
        json!([session_id])
    );

    let cli = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["session", "layout", "show"])
        .env("WEAVER_API", ts.addr.to_string())
        .output()
        .expect("running representative loom session layout command");
    assert!(
        cli.status.success(),
        "layout CLI failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let stdout = String::from_utf8_lossy(&cli.stdout);
    assert!(stdout.contains(&format!("session layout revision {move_revision}")));
    assert!(stdout.contains("Focused"));
    assert!(stdout.contains(session_id));

    ts.client
        .delete(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
}
