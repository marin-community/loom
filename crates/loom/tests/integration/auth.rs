//! Authentication wiring against a live server: loopback trust, bearer tokens,
//! the machine-local token, and password-login cookies.
//!
//! The harness connects from `127.0.0.1`, so loopback trust (on by default)
//! makes every other suite's bare requests work unchanged. Here we do the
//! trusted setup first, then flip `auth.trust_loopback` off and prove the three
//! credential paths gate access as designed.

use std::path::Path;

use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;

use super::fixtures::TestServer;

fn url(ts: &TestServer, path: &str) -> String {
    format!("http://{}{}", ts.addr, path)
}

#[tokio::test]
#[serial]
async fn loopback_trust_then_token_local_and_cookie_gate_access() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();

    // 1. Default: a loopback request is trusted as the seeded owner.
    let r = http.get(url(&ts, "/api/sessions")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let me: Value = http
        .get(url(&ts, "/api/auth/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["authenticated"], true);
    assert_eq!(me["username"], "rjpower");
    assert_eq!(me["via"], "loopback");
    assert_eq!(me["methods"]["password"], true);
    // No OAuth app configured in the test, so GitHub sign-in is off.
    assert_eq!(me["methods"]["github"], false);

    // 2. Trusted setup before locking down: mint a token and set a password.
    let created: Value = http
        .post(url(&ts, "/api/auth/tokens"))
        .json(&json!({ "name": "ci" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = created["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("loom_"), "token is prefixed: {token}");
    let token_id = created["id"].as_str().unwrap().to_string();

    let r = http
        .post(url(&ts, "/api/auth/password"))
        .json(&json!({ "new_password": "correct horse" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    // 3. Lock it down: stop trusting loopback.
    let r = http
        .patch(url(&ts, "/api/settings"))
        .json(&json!({ "auth.trust_loopback": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 4a. A bare request is now rejected.
    let r = http.get(url(&ts, "/api/sessions")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // 4b. The bearer token works.
    let r = http
        .get(url(&ts, "/api/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 4c. The machine-local token (what loom injects into its subprocesses) works
    //     too — that is what keeps the agent and overlookers running with trust off.
    let home = std::env::var("WEAVER_HOME").unwrap();
    let local = std::fs::read_to_string(Path::new(&home).join("loom-token")).unwrap();
    let r = http
        .get(url(&ts, "/api/sessions"))
        .bearer_auth(local.trim())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 4d. A password login yields a working session cookie.
    let login = http
        .post(url(&ts, "/api/auth/login"))
        .json(&json!({ "username": "rjpower", "password": "correct horse" }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let set_cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.contains("loom_session="));
    assert!(set_cookie.contains("HttpOnly"));
    let cookie_pair = set_cookie.split(';').next().unwrap().to_string();
    let r = http
        .get(url(&ts, "/api/sessions"))
        .header("cookie", &cookie_pair)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 4e. A wrong password is rejected.
    let r = http
        .post(url(&ts, "/api/auth/login"))
        .json(&json!({ "username": "rjpower", "password": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // 5. Revoking the token invalidates it immediately.
    let r = http
        .delete(url(&ts, &format!("/api/auth/tokens/{token_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let r = http
        .get(url(&ts, "/api/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn health_is_public_but_protected_routes_are_not() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();

    // Lock down loopback so the gate is in force.
    http.patch(url(&ts, "/api/settings"))
        .json(&json!({ "auth.trust_loopback": false }))
        .send()
        .await
        .unwrap();

    // Health stays public (liveness probes must not need a token).
    let r = http.get(url(&ts, "/api/health")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // /api/auth/me stays public, and now reports an unauthenticated caller.
    let me: Value = http
        .get(url(&ts, "/api/auth/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["authenticated"], false);

    // A protected route is gated.
    let r = http.get(url(&ts, "/api/branches")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}
