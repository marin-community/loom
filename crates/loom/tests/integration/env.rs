//! Operator-managed agent env vars over HTTP: the `settings.env.*` operations
//! and their name validation.
//!
//! These return the refreshed list directly. The legacy routes wrapped it in an
//! `{env: [...]}` envelope, which carried no information the caller did not
//! already have.

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_crud_and_name_validation() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Starts empty.
    let env = client.post("/api/settings/env/list", json!({})).await.unwrap();
    assert_eq!(env.as_array().unwrap().len(), 0, "env starts empty");
    let initial_revision = profile_revision(client, "default").await;

    // Upsert two; the reply is the refreshed, name-ordered list.
    client
        .post(
            "/api/settings/env/set",
            json!({ "name": "GH_HOST", "value": "github.example.com" }),
        )
        .await
        .unwrap();
    let after_put_revision = profile_revision(client, "default").await;
    assert_eq!(after_put_revision, initial_revision + 1);
    let env = client
        .post(
            "/api/settings/env/set",
            json!({ "name": "API_TOKEN", "value": "secret" }),
        )
        .await
        .unwrap();
    let list = env.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["name"], "API_TOKEN");
    assert_eq!(list[1]["name"], "GH_HOST");
    assert_eq!(list[1]["value"], "github.example.com");

    // Upsert replaces in place rather than adding a row.
    let env = client
        .post(
            "/api/settings/env/set",
            json!({ "name": "GH_HOST", "value": "github.internal" }),
        )
        .await
        .unwrap();
    let list = env.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1]["value"], "github.internal");

    // A non-identifier name is rejected.
    let err = client
        .post(
            "/api/settings/env/set",
            json!({ "name": "BAD-NAME", "value": "x" }),
        )
        .await;
    assert!(err.is_err(), "a non-identifier name must be rejected");

    // Delete one; the other remains.
    let env = client
        .post("/api/settings/env/delete", json!({ "name": "API_TOKEN" }))
        .await
        .unwrap();
    let list = env.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "GH_HOST");
    let after_delete_revision = profile_revision(client, "default").await;
    assert_eq!(after_delete_revision, after_put_revision + 3);

    // Deleting an absent name is a no-op, not an error.
    let env = client
        .post("/api/settings/env/delete", json!({ "name": "API_TOKEN" }))
        .await
        .unwrap();
    assert_eq!(env.as_array().unwrap().len(), 1);
}

/// A profile's monotonic revision, which every env mutation bumps.
async fn profile_revision(client: &weaver_api::Client, name: &str) -> i64 {
    client
        .post("/api/profiles/get", json!({ "name": name }))
        .await
        .unwrap()["revision"]
        .as_i64()
        .unwrap()
}
