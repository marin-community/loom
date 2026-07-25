//! Operator-managed agent env vars over HTTP: the `/api/env` CRUD surface and
//! its name validation.

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_crud_and_name_validation() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Starts empty.
    let env = client.get("/api/env").await.unwrap();
    assert_eq!(env["env"].as_array().unwrap().len(), 0, "env starts empty");
    let initial_revision = client.get("/api/profiles/default").await.unwrap()["revision"]
        .as_i64()
        .unwrap();

    // Upsert two; the reply is the refreshed, name-ordered list.
    client
        .put("/api/env/GH_HOST", json!({ "value": "github.example.com" }))
        .await
        .unwrap();
    let after_put_revision = client.get("/api/profiles/default").await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    assert_eq!(after_put_revision, initial_revision + 1);
    let env = client
        .put("/api/env/API_TOKEN", json!({ "value": "secret" }))
        .await
        .unwrap();
    let list = env["env"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["name"], "API_TOKEN");
    assert_eq!(list[1]["name"], "GH_HOST");
    assert_eq!(list[1]["value"], "github.example.com");

    // Upsert replaces in place rather than adding a row.
    let env = client
        .put("/api/env/GH_HOST", json!({ "value": "github.internal" }))
        .await
        .unwrap();
    let list = env["env"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1]["value"], "github.internal");

    // A non-identifier name is rejected.
    let err = client
        .put("/api/env/BAD-NAME", json!({ "value": "x" }))
        .await;
    assert!(err.is_err(), "a non-identifier name must be rejected");

    // Delete one; the other remains.
    let env = client.delete("/api/env/API_TOKEN").await.unwrap();
    let list = env["env"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "GH_HOST");
    let after_delete_revision = client.get("/api/profiles/default").await.unwrap()["revision"]
        .as_i64()
        .unwrap();
    assert_eq!(after_delete_revision, after_put_revision + 3);

    // Deleting an absent name is a no-op, not an error.
    let env = client.delete("/api/env/API_TOKEN").await.unwrap();
    assert_eq!(env["env"].as_array().unwrap().len(), 1);
}
