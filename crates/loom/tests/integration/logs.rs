//! The human log viewer's HTTP surface: `diagnostics.status`, the `logs.list`
//! snapshot, and the `/api/logs/stream` tail. The security-critical properties
//! are that all three require a human role and user-role messages are redacted.
//! This suite proves the HTTP shape and auth boundary; redaction is exercised by
//! the `loom::logs` unit tests.
//!
//! (The ring-buffer *capture* is exercised by the `loom::logs` unit tests and the
//! e2e suite against the real binary, which is where the tracing layer is
//! installed — the integration harness builds the app without it.)

use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;

use super::fixtures::TestServer;

fn url(ts: &TestServer, path: &str) -> String {
    format!("http://{}{}", ts.addr, path)
}

#[tokio::test]
#[serial]
async fn status_and_logs_are_shaped_and_human_only() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();

    // Loopback-trusted (the harness connects from 127.0.0.1): reachable + shaped.
    let st: Value = http
        .post(url(&ts, "/api/diagnostics/status"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(st["version"].as_str().is_some(), "status has a version");
    assert!(
        st["build_revision"]
            .as_str()
            .is_some_and(|revision| !revision.is_empty()),
        "status has a build revision or the explicit unknown sentinel"
    );
    assert!(
        st["build_profile"]
            .as_str()
            .is_some_and(|profile| !profile.is_empty()),
        "status has a cargo build profile"
    );
    assert!(
        st.get("image").is_some(),
        "status has a nullable runtime image identity"
    );
    assert!(st["pid"].as_u64().unwrap_or(0) > 0, "status has a pid");
    assert!(
        st["started_at"].as_str().unwrap_or("").len() >= 10,
        "status has an RFC3339 start time"
    );

    let logs: Value = http
        .post(url(&ts, "/api/logs/list"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(logs.is_array(), "logs snapshot is a JSON array");

    // `limit` is honored and clamped (never negative / never panics).
    let r = http
        .post(url(&ts, "/api/logs/list"))
        .json(&json!({ "limit": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.json::<Value>().await.unwrap().is_array());

    // Lock down loopback trust — every subsequent bare request needs a credential.
    let r = http
        .post(url(&ts, "/api/settings/patch"))
        .json(&json!({ "changes": { "auth.trust_loopback": false } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // All three log endpoints are now operator-gated.
    let r = http
        .post(url(&ts, "/api/diagnostics/status"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "/api/diagnostics/status must require auth"
    );
    let r = http
        .post(url(&ts, "/api/logs/list"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "/api/logs/list must require auth"
    );
    let r = http.get(url(&ts, "/api/logs/stream")).send().await.unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "/api/logs/stream must require auth"
    );
}
