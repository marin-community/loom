//! Authentication wiring against a live server: loopback trust, bearer tokens,
//! the machine-local token, and password-login cookies.
//!
//! The harness connects from `127.0.0.1`, so loopback trust (on by default)
//! makes every other suite's bare requests work unchanged. Here we do the
//! trusted setup first, then flip `auth.trust_loopback` off and prove the three
//! credential paths gate access as designed.

use std::path::Path;
use std::process::Command;

use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;

use super::fixtures::TestServer;

fn url(ts: &TestServer, path: &str) -> String {
    format!("http://{}{}", ts.addr, path)
}

#[tokio::test]
#[serial]
async fn personal_github_token_is_self_service_and_write_only() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();
    let endpoint = url(&ts, "/api/auth/github-token");

    let initial: Value = http
        .get(&endpoint)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial, json!({ "set": false, "updated_at": null }));

    let stored: Value = http
        .put(&endpoint)
        .json(&json!({ "token": "github_pat_write_only" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stored["set"], true);
    assert!(stored["updated_at"].is_string());
    assert!(!stored.to_string().contains("github_pat_write_only"));
    assert_eq!(
        loom::user_token::get(&ts.state.db, "rjpower")
            .await
            .unwrap()
            .as_deref(),
        Some("github_pat_write_only")
    );

    let deleted = http.delete(&endpoint).send().await.unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(loom::user_token::get(&ts.state.db, "rjpower")
        .await
        .unwrap()
        .is_none());
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

    // 4c. The machine-local admin token still works for operator CLI and watch
    //     infrastructure. Agent sessions receive narrower session tokens.
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

    // An explicit invalid bearer is authoritative: it cannot fall through to
    // the otherwise-valid cookie (or loopback trust).
    let r = http
        .get(url(&ts, "/api/sessions"))
        .header("cookie", &cookie_pair)
        .bearer_auth("loom_revoked-or-invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

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
async fn user_role_keeps_operations_and_diagnostics_but_not_administration() {
    use futures_util::StreamExt;
    use tracing_subscriber::prelude::*;

    let ts = TestServer::start_api_only().await;
    let http = reqwest::Client::new();
    let added: Value = http
        .post(url(&ts, "/api/auth/users"))
        .json(&json!({ "username": "alice", "github_login": "alice-gh" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(added["role"], "user");
    let (user_token, _) = loom::auth::create_token(&ts.state.db, "alice", "alice-api", None)
        .await
        .unwrap();
    let (admin_token, _) = loom::auth::create_token(&ts.state.db, "rjpower", "admin-api", None)
        .await
        .unwrap();
    ts.client
        .patch("/api/settings", json!({ "auth.trust_loopback": false }))
        .await
        .unwrap();

    let me: Value = http
        .get(url(&ts, "/api/auth/me"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["role"], "user");

    const LOG_MARKER: &str = "user-redaction-check-6d93";
    const LOG_SECRET: &str = "opaque-deployment-credential-4c71";
    loom::profile::env_set(
        &ts.state.db,
        loom::profile::DEFAULT_PROFILE,
        "REDACTION_TEST_TOKEN",
        LOG_SECRET,
    )
    .await
    .unwrap();
    let subscriber = tracing_subscriber::registry().with(loom::logs::layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!("{LOG_MARKER} credential={LOG_SECRET}");
    });

    let user_logs: Value = http
        .get(url(&ts, "/api/logs"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user_log = user_logs
        .as_array()
        .unwrap()
        .iter()
        .find(|line| {
            line["message"]
                .as_str()
                .is_some_and(|message| message.contains(LOG_MARKER))
        })
        .unwrap();
    assert!(!user_log["message"].as_str().unwrap().contains(LOG_SECRET));

    let admin_logs: Value = http
        .get(url(&ts, "/api/logs"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let admin_log = admin_logs
        .as_array()
        .unwrap()
        .iter()
        .find(|line| {
            line["message"]
                .as_str()
                .is_some_and(|message| message.contains(LOG_MARKER))
        })
        .unwrap();
    assert!(admin_log["message"].as_str().unwrap().contains(LOG_SECRET));

    const STREAM_MARKER: &str = "user-stream-redaction-check-1e52";
    let stream_response = http
        .get(url(&ts, "/api/events?topics=logs"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(stream_response.status(), StatusCode::OK);
    tracing::subscriber::with_default(
        tracing_subscriber::registry().with(loom::logs::layer()),
        || tracing::warn!("{STREAM_MARKER} credential={LOG_SECRET}"),
    );
    let mut stream = stream_response.bytes_stream();
    let mut stream_body = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !stream_body.contains(STREAM_MARKER) && tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Some(chunk) = tokio::time::timeout(remaining, stream.next())
            .await
            .unwrap()
            .transpose()
            .unwrap()
        else {
            break;
        };
        stream_body.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(stream_body.contains(STREAM_MARKER), "{stream_body}");
    assert!(!stream_body.contains(LOG_SECRET), "{stream_body}");

    for path in [
        "/api/sessions",
        "/api/settings",
        "/api/profiles",
        "/api/agents",
        "/api/mcps",
        "/api/diagnostics",
        "/api/logs",
        "/api/status",
        "/api/tasks",
        "/api/session-layout",
        "/api/watches",
        "/api/watches/programs",
    ] {
        let response = http
            .get(url(&ts, path))
            .bearer_auth(&user_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "user GET {path}");
    }

    let preferences: Value = http
        .patch(url(&ts, "/api/preferences"))
        .bearer_auth(&user_token)
        .json(&json!({ "terminal.theme": "light" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let theme = preferences["preferences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|preference| preference["key"] == "terminal.theme")
        .unwrap();
    assert_eq!(theme["value"], "light");
    assert_eq!(theme["is_overridden"], true);

    let admin_preferences: Value = http
        .get(url(&ts, "/api/preferences"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let admin_theme = admin_preferences["preferences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|preference| preference["key"] == "terminal.theme")
        .unwrap();
    assert_eq!(admin_theme["value"], "dark");
    assert_eq!(admin_theme["is_overridden"], false);

    let tokens: Vec<Value> = http
        .get(url(&ts, "/api/auth/tokens"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0]["name"], "alice-api");

    let forbidden = [
        (reqwest::Method::PATCH, "/api/settings"),
        (reqwest::Method::POST, "/api/deployment/reconcile"),
        (reqwest::Method::GET, "/api/auth/users"),
        (reqwest::Method::GET, "/api/auth/github/config"),
        (reqwest::Method::POST, "/api/auth/automation-token"),
        (reqwest::Method::GET, "/api/auth/federations"),
        (reqwest::Method::POST, "/api/profiles"),
        (reqwest::Method::POST, "/api/agents/custom"),
        (reqwest::Method::POST, "/api/mcps/custom"),
        (reqwest::Method::PUT, "/api/env/SHARED_VALUE"),
        (reqwest::Method::GET, "/api/shell/terminal"),
        (reqwest::Method::POST, "/api/shell/restart"),
        (reqwest::Method::POST, "/api/watches"),
        (reqwest::Method::POST, "/api/watches/status/run"),
    ];
    for (method, path) in forbidden {
        let response = http
            .request(method, url(&ts, path))
            .bearer_auth(&user_token)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "user request {path}"
        );
    }

    let promoted: Value = http
        .put(url(&ts, "/api/auth/users/alice/role"))
        .bearer_auth(&admin_token)
        .json(&json!({ "role": "admin" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(promoted["role"], "admin");
    let response = http
        .patch(url(&ts, "/api/settings"))
        .bearer_auth(&user_token)
        .json(&json!({ "terminal.theme": "light" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
#[serial]
async fn absurd_token_expiry_is_a_bad_request_not_a_panic() {
    let ts = TestServer::start().await;
    let response = reqwest::Client::new()
        .post(url(&ts, "/api/auth/tokens"))
        .json(&json!({ "name": "too-long", "expires_in_days": i64::MAX }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

#[tokio::test]
#[serial]
async fn every_registered_operation_is_mounted_by_an_api_bundle_factory() {
    let ts = TestServer::start().await;
    ts.client
        .patch("/api/settings", json!({ "auth.trust_loopback": false }))
        .await
        .unwrap();
    let http = reqwest::Client::new();

    for operation in weaver_api::operations() {
        let path = operation
            .path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') && segment.ends_with('}') {
                    "factory-probe"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let response = http
            .request(operation.method.parse().unwrap(), url(&ts, &path))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{} {} for operation {} was not mounted behind auth",
            operation.method,
            operation.path,
            operation.id
        );
    }
}

#[tokio::test]
#[serial]
async fn session_token_is_limited_to_its_tree_and_repository_work_items() {
    let ts = TestServer::start().await;
    let explicit = weaver_core::issue::add(
        &ts.state.db,
        &weaver_core::issue::NewIssue {
            repo_root: ts.cwd(),
            title: "explicit scoped work".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let created = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "cwd": ts.cwd(),
                "goal": "scoped parent",
                "agent": "shell",
                "claim_issue": explicit.id
            }),
        )
        .await
        .unwrap();
    let session_id = created["id"].as_str().unwrap();
    let branch_id = created["branch"]["id"].as_str().unwrap();
    let tracking_issue = created["tracking_issue"].as_i64().unwrap();
    let token =
        loom::auth::create_session_token(&ts.state.db, Some("rjpower"), session_id, branch_id)
            .await
            .unwrap();
    let sibling = ts
        .client
        .post(
            "/api/sessions",
            json!({ "cwd": ts.cwd(), "goal": "scoped sibling", "agent": "shell" }),
        )
        .await
        .unwrap();
    let sibling_id = sibling["id"].as_str().unwrap().to_string();

    let unrelated = weaver_core::issue::add(
        &ts.state.db,
        &weaver_core::issue::NewIssue {
            repo_root: ts.cwd(),
            title: "unrelated backlog".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let foreign = weaver_core::issue::add(
        &ts.state.db,
        &weaver_core::issue::NewIssue {
            repo_root: format!("{}-foreign", ts.cwd()),
            title: "foreign repository backlog".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ts.client
        .patch("/api/settings", json!({ "auth.trust_loopback": false }))
        .await
        .unwrap();
    let http = reqwest::Client::new();

    let own = http
        .get(url(&ts, &format!("/api/sessions/{session_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    let own_channel = http
        .get(url(&ts, &format!("/api/channels/{session_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(own_channel.status(), StatusCode::OK);
    let issue = http
        .get(url(&ts, &format!("/api/issues/{tracking_issue}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), StatusCode::OK);
    let own_history = http
        .get(url(&ts, &format!("/api/sessions/{session_id}/history")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(own_history.status(), StatusCode::OK);

    let child = http
        .post(url(&ts, "/api/sessions"))
        .bearer_auth(&token)
        .json(&json!({ "cwd": ts.cwd(), "goal": "scoped child", "agent": "shell" }))
        .send()
        .await
        .unwrap();
    assert_eq!(child.status(), StatusCode::OK);
    let child: Value = child.json().await.unwrap();
    let child_id = child["id"].as_str().unwrap();
    let child_row = loom::session::get(&ts.state.db, child["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_row.parent_session_id.as_deref(), Some(session_id));
    let child_channel = http
        .get(url(&ts, &format!("/api/channels/{child_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(child_channel.status(), StatusCode::OK);
    let custom = http
        .post(url(&ts, "/api/channels"))
        .bearer_auth(&token)
        .json(&json!({ "name": "shared review", "topic": "explicit pipe" }))
        .send()
        .await
        .unwrap();
    assert_eq!(custom.status(), StatusCode::CREATED);
    let custom: Value = custom.json().await.unwrap();
    let custom_id = custom["id"].as_str().unwrap();
    let invite = http
        .put(url(&ts, &format!("/api/channels/{custom_id}/subscription")))
        .bearer_auth(&token)
        .json(&json!({ "mode": "deliver", "session_id": child_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(invite.status(), StatusCode::OK);
    let invite: Value = invite.json().await.unwrap();
    assert_eq!(invite["subject_id"], child_id);
    assert_eq!(invite["mode"], "deliver");
    let visible_channels = http
        .get(url(&ts, "/api/channels"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(visible_channels.status(), StatusCode::OK);
    let visible_channels: Vec<Value> = visible_channels.json().await.unwrap();
    let visible_ids = visible_channels
        .iter()
        .filter_map(|channel| channel["id"].as_str())
        .collect::<Vec<_>>();
    assert!(visible_ids.contains(&session_id));
    assert!(visible_ids.contains(&child_id));
    assert!(visible_ids.contains(&custom_id));
    assert!(!visible_ids.contains(&sibling_id.as_str()));
    let child_token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        child_id,
        &child_row.branch_id,
    )
    .await
    .unwrap();
    let invited_archive = http
        .delete(url(&ts, &format!("/api/channels/{custom_id}")))
        .bearer_auth(&child_token)
        .send()
        .await
        .unwrap();
    assert_eq!(invited_archive.status(), StatusCode::FORBIDDEN);
    let sibling_channel = http
        .get(url(&ts, &format!("/api/channels/{sibling_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(sibling_channel.status(), StatusCode::FORBIDDEN);
    let creator_archive = http
        .delete(url(&ts, &format!("/api/channels/{custom_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(creator_archive.status(), StatusCode::OK);

    let unrelated = http
        .get(url(&ts, &format!("/api/issues/{}", unrelated.id)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(unrelated.status(), StatusCode::OK);
    let foreign = http
        .get(url(&ts, &format!("/api/issues/{}", foreign.id)))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    for path in [
        format!("/api/sessions/{sibling_id}/history"),
        format!("/api/sessions/{sibling_id}/history/search?q=secret"),
    ] {
        let sibling_history = http
            .get(url(&ts, &path))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            sibling_history.status(),
            StatusCode::FORBIDDEN,
            "session token read sibling history through {path}"
        );
    }
    let admin = http
        .get(url(&ts, "/api/auth/tokens"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin.status(), StatusCode::FORBIDDEN);

    let automation = loom::automation::mint(
        &ts.state.db,
        "ci",
        vec!["github_comment".to_string()],
        60,
        None,
    )
    .await
    .unwrap();
    for credential in [&token, &automation.token] {
        let layout = http
            .get(url(&ts, "/api/session-layout"))
            .bearer_auth(credential)
            .send()
            .await
            .unwrap();
        assert_eq!(layout.status(), StatusCode::FORBIDDEN);
        let mutation = http
            .post(url(&ts, "/api/session-layout/moves"))
            .bearer_auth(credential)
            .json(&json!({
                "session_ids": [session_id],
                "destination_group_id": "group-user-inbox",
                "expected_revision": 1
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(mutation.status(), StatusCode::FORBIDDEN);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn session_token_can_delegate_through_the_cli_resolve_then_create_path() {
    let ts = TestServer::start().await;
    let parent = ts
        .client
        .post(
            "/api/sessions",
            json!({ "cwd": ts.cwd(), "goal": "scoped parent", "agent": "shell" }),
        )
        .await
        .unwrap();
    let parent_id = parent["id"].as_str().unwrap();
    let parent_branch_id = parent["branch"]["id"].as_str().unwrap();
    let token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        parent_id,
        parent_branch_id,
    )
    .await
    .unwrap();
    ts.client
        .patch("/api/settings", json!({ "auth.trust_loopback": false }))
        .await
        .unwrap();

    // The session tree spans repositories. Branch ancestry and work-item
    // provenance remain repository-scoped, but the authenticated launcher is
    // still the exact parent of a child created elsewhere.
    let other_repo = tempfile::tempdir().unwrap();
    crate::fixtures::sh(other_repo.path(), "git", &["init", "-b", "main"]);
    crate::fixtures::sh(
        other_repo.path(),
        "git",
        &["config", "user.email", "t@t.test"],
    );
    crate::fixtures::sh(other_repo.path(), "git", &["config", "user.name", "Test"]);
    std::fs::write(other_repo.path().join("README.md"), "other\n").unwrap();
    crate::fixtures::sh(other_repo.path(), "git", &["add", "."]);
    crate::fixtures::sh(other_repo.path(), "git", &["commit", "-m", "init"]);
    let cwd = other_repo.path().to_string_lossy().into_owned();
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "session",
            "launch",
            "--repo",
            &cwd,
            "--agent",
            "shell",
            "delegated through scoped CLI",
        ])
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .env("WEAVER_BRANCH", parent_branch_id)
        .env("LOOM_TOKEN", token)
        .output()
        .expect("running scoped loom session launch");
    assert!(
        output.status.success(),
        "scoped launch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let children = loom::session::list(&ts.state.db).await.unwrap();
    let child = children
        .iter()
        .find(|session| session.parent_session_id.as_deref() == Some(parent_id))
        .expect("cross-repo CLI launch created a child of the scoped session");
    assert_eq!(child.creator_kind, "session");
    assert_eq!(
        child.parent_branch_id, None,
        "legacy branch ancestry remains repository-scoped"
    );
}
