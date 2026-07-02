//! The operator log viewer's HTTP surface: `/api/status`, the `/api/logs`
//! snapshot, and the `/api/logs/stream` tail. The security-critical property is
//! that all three are operator-only (server logs can carry secrets), so we prove
//! the shape while loopback-trusted, then lock loopback down and prove they 401.
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
async fn status_and_logs_are_shaped_and_operator_only() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();

    // Loopback-trusted (the harness connects from 127.0.0.1): reachable + shaped.
    let st: Value = http
        .get(url(&ts, "/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(st["version"].as_str().is_some(), "status has a version");
    assert!(st["pid"].as_u64().unwrap_or(0) > 0, "status has a pid");
    assert!(
        st["started_at"].as_str().unwrap_or("").len() >= 10,
        "status has an RFC3339 start time"
    );

    let logs: Value = http
        .get(url(&ts, "/api/logs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(logs.is_array(), "logs snapshot is a JSON array");

    // `limit` is honored and clamped (never negative / never panics).
    let r = http
        .get(url(&ts, "/api/logs?limit=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.json::<Value>().await.unwrap().is_array());

    // Lock down loopback trust — every subsequent bare request needs a credential.
    let r = http
        .patch(url(&ts, "/api/settings"))
        .json(&json!({ "auth.trust_loopback": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // All three log endpoints are now operator-gated.
    for path in ["/api/status", "/api/logs", "/api/logs/stream"] {
        let r = http.get(url(&ts, path)).send().await.unwrap();
        assert_eq!(
            r.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must require auth"
        );
    }
}
