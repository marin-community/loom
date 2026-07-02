//! Custom agents over HTTP: the `/api/agents/custom` CRUD surface, how the
//! definitions merge into `/api/agents`, and launching a session with one.

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

/// Find an agent by kind in the `/api/agents` picker list.
fn find<'a>(agents: &'a serde_json::Value, kind: &str) -> Option<&'a serde_json::Value> {
    agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == kind)
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_list_merges_builtins_and_custom() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let res = client.get("/api/agents").await.unwrap();
    let agents = &res["agents"];
    // Builtins are present and flagged as such.
    let claude = find(agents, "claude").expect("claude is builtin");
    assert_eq!(claude["builtin"], true);
    assert!(find(agents, "codex").is_some(), "codex is builtin");
    // The old builtin "shell" agent is gone; the test fixture seeds it as a
    // custom agent instead, so it shows up as non-builtin.
    let shell = find(agents, "shell").expect("fixture seeds a custom shell agent");
    assert_eq!(shell["builtin"], false);
    // The full custom definitions ride alongside the picker list.
    let custom = res["custom"].as_array().unwrap();
    assert!(custom.iter().any(|a| a["name"] == "shell"));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_agent_crud_and_validation() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Create one. The reply is the refreshed custom list.
    let res = client
        .post(
            "/api/agents/custom",
            json!({
                "name": "aider",
                "label": "Aider",
                "setup": "echo hooks",
                "launch": "aider --message",
                "resume": "aider --continue",
                "reports_status": false,
            }),
        )
        .await
        .unwrap();
    let custom = res["custom"].as_array().unwrap();
    let aider = custom.iter().find(|a| a["name"] == "aider").unwrap();
    assert_eq!(aider["label"], "Aider");
    assert_eq!(aider["launch"], "aider --message");

    // It now appears in the merged picker list as a non-builtin.
    let list = client.get("/api/agents").await.unwrap();
    let picked = find(&list["agents"], "aider").expect("custom agent in picker");
    assert_eq!(picked["builtin"], false);
    assert_eq!(picked["label"], "Aider");

    // A reserved builtin name is rejected.
    assert!(
        client
            .post(
                "/api/agents/custom",
                json!({ "name": "claude", "label": "X", "launch": "x" })
            )
            .await
            .is_err(),
        "a builtin name must be rejected"
    );
    // A malformed slug is rejected.
    assert!(
        client
            .post(
                "/api/agents/custom",
                json!({ "name": "has space", "label": "X", "launch": "x" })
            )
            .await
            .is_err(),
        "a non-slug name must be rejected"
    );
    // A missing label is rejected (a command-less agent is fine — it's a bare
    // shell — but it still needs a label).
    assert!(
        client
            .post("/api/agents/custom", json!({ "name": "empty" }))
            .await
            .is_err(),
        "a label-less agent must be rejected"
    );
    // A duplicate name is a conflict.
    assert!(
        client
            .post(
                "/api/agents/custom",
                json!({ "name": "aider", "label": "Dup", "launch": "x" })
            )
            .await
            .is_err(),
        "a duplicate name must be rejected"
    );

    // Update it in place (the name is immutable, taken from the path).
    let res = client
        .put(
            "/api/agents/custom/aider",
            json!({
                "label": "Aider v2",
                "setup": "",
                "launch": "aider",
                "resume": "",
                "reports_status": true,
            }),
        )
        .await
        .unwrap();
    let aider = res["custom"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "aider")
        .unwrap();
    assert_eq!(aider["label"], "Aider v2");
    assert_eq!(aider["reports_status"], true);

    // Updating an unknown (or builtin) name is a 404.
    assert!(
        client
            .put(
                "/api/agents/custom/claude",
                json!({ "label": "X", "launch": "x" })
            )
            .await
            .is_err(),
        "updating a builtin name must fail"
    );

    // Delete it; it leaves the list. Deleting again is a no-op.
    let res = client.delete("/api/agents/custom/aider").await.unwrap();
    assert!(
        !res["custom"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "aider"),
        "aider is gone after delete"
    );
    assert!(
        client.delete("/api/agents/custom/aider").await.is_ok(),
        "deleting an absent agent is a no-op"
    );
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_a_session_with_a_custom_agent() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // A custom agent whose launch command is a real, harmless binary, so the
    // launch script runs cleanly and the session comes up.
    client
        .post(
            "/api/agents/custom",
            json!({ "name": "noop", "label": "Noop", "launch": "true", "reports_status": false }),
        )
        .await
        .unwrap();

    let session = client
        .post(
            "/api/sessions",
            json!({ "goal": "hi", "cwd": ts.cwd(), "agent": "noop" }),
        )
        .await
        .unwrap();
    assert_eq!(session["agent_kind"], "noop");
    // A hookless custom agent is live on launch, not stuck at `launching`.
    assert_eq!(session["status"], "running");

    // A session requesting an unknown agent is rejected.
    assert!(
        client
            .post(
                "/api/sessions",
                json!({ "goal": "hi", "cwd": ts.cwd(), "agent": "ghost" }),
            )
            .await
            .is_err(),
        "an unknown agent must be rejected at create time"
    );
}
