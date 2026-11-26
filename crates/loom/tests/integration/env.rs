//! Operator-managed agent env vars over HTTP: the `settings.env.*` operations
//! and their name validation.
//!
//! These return the refreshed list directly.

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_crud_and_name_validation() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Starts empty.
    let env = client
        .post("/api/settings/env/list", json!({}))
        .await
        .unwrap();
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

/// Opencode's configuration travels in `OPENCODE_CONFIG_CONTENT` — the
/// environment variable opencode itself reads — so it is an ordinary profile
/// variable: the value stored through either env API is exactly what the next
/// probe and launch export, and a non-default profile can carry its own.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_config_content_is_an_ordinary_profile_variable() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // The settings facade stores it on the default profile, value visible.
    let env = client
        .post(
            "/api/settings/env/set",
            json!({
                "name": "OPENCODE_CONFIG_CONTENT",
                "value": r#"{"provider":{"lunaroute":{"options":{"apiKey":"{env:OPENCODE_API_KEY}"}}}}"#
            }),
        )
        .await
        .unwrap();
    let list = env.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "OPENCODE_CONFIG_CONTENT");
    assert!(list[0]["value"].as_str().unwrap().contains("lunaroute"));

    // And a second profile can carry its own configuration — credentials are
    // per-profile, not a shared file only the default profile may own.
    client
        .post(
            "/api/profiles/create",
            json!({
                "name": "second",
                "agent_kind": "opencode",
                "ambient_allowlist": [],
                "github_repositories": []
            }),
        )
        .await
        .unwrap();
    client
        .post(
            "/api/profiles/env/set",
            json!({
                "profile": "second",
                "name": "OPENCODE_CONFIG_CONTENT",
                "value": r#"{"provider":{"lunaroute":{"options":{"apiKey":"sk-other"}}}}"#
            }),
        )
        .await
        .expect("a non-default profile may carry its own Opencode config");

    // Deleting the default profile's value leaves the second profile's intact.
    let env = client
        .post(
            "/api/settings/env/delete",
            json!({ "name": "OPENCODE_CONFIG_CONTENT" }),
        )
        .await
        .unwrap();
    assert_eq!(env.as_array().unwrap().len(), 0);
    let profile = client
        .post("/api/profiles/get", json!({ "name": "second" }))
        .await
        .unwrap();
    let names: Vec<&str> = profile["env"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert!(names.contains(&"OPENCODE_CONFIG_CONTENT"));
}

/// `OPENCODE_CREDENTIALS_JSON` is a boot input the server imports into
/// Opencode's shared store — it must never become a session variable, whose
/// value would be exported into every launched session.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_credentials_json_is_rejected_as_a_profile_variable() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let err = client
        .post(
            "/api/settings/env/set",
            json!({ "name": "OPENCODE_CREDENTIALS_JSON", "value": "[]" }),
        )
        .await
        .expect_err("the boot-only credential must not become a session variable");
    assert!(
        err.to_string()
            .contains("can only be set for Loom's own boot provisioning"),
        "unexpected error: {err}"
    );

    client
        .post(
            "/api/profiles/create",
            json!({
                "name": "second",
                "agent_kind": "opencode",
                "ambient_allowlist": [],
                "github_repositories": []
            }),
        )
        .await
        .unwrap();
    let err = client
        .post(
            "/api/profiles/env/set",
            json!({
                "profile": "second",
                "name": "OPENCODE_CREDENTIALS_JSON",
                "value": "[]",
            }),
        )
        .await
        .expect_err("the boot-only credential must not become a profile variable");
    assert!(
        err.to_string()
            .contains("can only be set for Loom's own boot provisioning"),
        "unexpected error: {err}"
    );

    // The repository-scoped env API is a third way into a session environment.
    let err = client
        .post(
            "/api/repos/env/set",
            json!({
                "name": "OPENCODE_CREDENTIALS_JSON",
                "value": "[]",
                "cwd": ts.cwd(),
            }),
        )
        .await
        .expect_err("the boot-only credential must not become a repo variable");
    assert!(
        err.to_string()
            .contains("can only be set for Loom's own boot provisioning"),
        "unexpected error: {err}"
    );
}
