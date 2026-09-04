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
async fn organization_revalidation_renews_only_active_members() {
    let ts = TestServer::start_with_app().await;
    weaver_core::config::apply(
        &ts.state.db,
        &[(
            loom::auth::GH_ORGANIZATIONS_KEY.to_string(),
            Some("broken:302, Alternate:304".to_string()),
        )],
    )
    .await
    .unwrap();
    for (username, github_login, github_user_id) in [
        ("renamed-account", "old-name", 505),
        ("reclaimed-name", "member", 606),
        ("former-member", "former-member", 405),
    ] {
        sqlx::query(
            "INSERT INTO users
             (username, github_login, github_user_id, role, authorization_kind,
              authorization_github_org_id, authorization_github_org_login,
              authorization_valid_until)
             VALUES (?, ?, ?, 'user', 'github_organization', 303, 'Acme',
                     '2000-01-01T00:00:00.000Z')",
        )
        .bind(username)
        .bind(github_login)
        .bind(github_user_id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    }

    loom::server::revalidate_github_authorizations_once(&ts.state).await;

    let now = weaver_core::db::now_iso();
    let active = loom::auth::get_user(&ts.state.db, "renamed-account")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.github_login.as_deref(), Some("renamed-member"));
    assert!(active.authorization_valid_until.as_deref().unwrap() > now.as_str());
    assert_eq!(active.authorization_github_org_id, Some(304));
    assert_eq!(
        active.authorization_github_org_login.as_deref(),
        Some("Alternate")
    );
    let reclaimed = loom::auth::get_user(&ts.state.db, "reclaimed-name")
        .await
        .unwrap()
        .unwrap();
    assert!(reclaimed.authorization_valid_until.as_deref().unwrap() <= now.as_str());
    let inactive = loom::auth::get_user(&ts.state.db, "former-member")
        .await
        .unwrap()
        .unwrap();
    assert!(inactive.authorization_valid_until.as_deref().unwrap() <= now.as_str());
}

#[tokio::test]
#[serial]
async fn personal_github_token_is_self_service_and_write_only() {
    let ts = TestServer::start().await;
    let http = reqwest::Client::new();
    let get_endpoint = url(&ts, "/api/auth/github_token/get");
    let set_endpoint = url(&ts, "/api/auth/github_token/set");
    let remove_endpoint = url(&ts, "/api/auth/github_token/remove");

    let initial: Value = http
        .post(&get_endpoint)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial, json!({ "set": false, "updated_at": null }));

    let stored: Value = http
        .post(&set_endpoint)
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

    let deleted = http
        .post(&remove_endpoint)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted: Value = deleted.json().await.unwrap();
    assert_eq!(deleted, json!({ "set": false, "updated_at": null }));
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
    let r = http
        .post(url(&ts, "/api/sessions/list"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let me: Value = http
        .post(url(&ts, "/api/auth/me"))
        .json(&json!({}))
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
        .post(url(&ts, "/api/auth/tokens/create"))
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
        .post(url(&ts, "/api/auth/set_password"))
        .json(&json!({ "new_password": "correct horse" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 3. Lock it down: stop trusting loopback.
    let r = http
        .post(url(&ts, "/api/settings/patch"))
        .json(&json!({ "changes": { "auth.trust_loopback": false } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 4a. A bare request is now rejected.
    let r = http
        .post(url(&ts, "/api/sessions/list"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // 4b. The bearer token works.
    let r = http
        .post(url(&ts, "/api/sessions/list"))
        .bearer_auth(&token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 4c. The machine-local admin token still works for operator CLI and watch
    //     infrastructure. Agent sessions receive narrower session tokens.
    let home = std::env::var("WEAVER_HOME").unwrap();
    let local = std::fs::read_to_string(Path::new(&home).join("loom-token")).unwrap();
    let r = http
        .post(url(&ts, "/api/sessions/list"))
        .bearer_auth(local.trim())
        .json(&json!({}))
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
        .post(url(&ts, "/api/sessions/list"))
        .header("cookie", &cookie_pair)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // An explicit invalid bearer is authoritative: it cannot fall through to
    // the otherwise-valid cookie (or loopback trust).
    let r = http
        .post(url(&ts, "/api/sessions/list"))
        .header("cookie", &cookie_pair)
        .bearer_auth("loom_revoked-or-invalid")
        .json(&json!({}))
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
        .post(url(&ts, "/api/auth/tokens/revoke"))
        .bearer_auth(&token)
        .json(&json!({ "id": token_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = http
        .post(url(&ts, "/api/sessions/list"))
        .bearer_auth(&token)
        .json(&json!({}))
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
        .post(url(&ts, "/api/auth/users/create"))
        .json(&json!({
            "username": "alice",
            "github_login": "alice-gh",
            "github_user_id": 101
        }))
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
        .post(
            "/api/settings/patch",
            json!({ "changes": { "auth.trust_loopback": false } }),
        )
        .await
        .unwrap();

    let me: Value = http
        .post(url(&ts, "/api/auth/me"))
        .json(&json!({}))
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
        .post(url(&ts, "/api/logs/list"))
        .bearer_auth(&user_token)
        .json(&json!({}))
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
        .post(url(&ts, "/api/logs/list"))
        .bearer_auth(&admin_token)
        .json(&json!({}))
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
        .get(url(&ts, "/api/events/stream?topics=logs"))
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

    // These are `User`-reachable operations read via `POST` with an empty body.
    for (path, body) in [
        ("/api/sessions/list", json!({})),
        ("/api/agents/list", json!({})),
        ("/api/diagnostics/get", json!({})),
        ("/api/logs/list", json!({})),
        ("/api/diagnostics/status", json!({})),
        ("/api/tasks/list", json!({})),
        ("/api/session_layout/get", json!({})),
        ("/api/watches/list", json!({})),
        ("/api/watches/programs", json!({})),
        ("/api/profiles/list", json!({})),
        ("/api/mcps/get", json!({})),
        ("/api/settings/get", json!({})),
    ] {
        let response = http
            .post(url(&ts, path))
            .bearer_auth(&user_token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "user POST {path}");
    }

    let preferences: Value = http
        .post(url(&ts, "/api/preferences/patch"))
        .bearer_auth(&user_token)
        .json(&json!({ "changes": { "terminal.theme": "light" } }))
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
        .post(url(&ts, "/api/preferences/get"))
        .bearer_auth(&admin_token)
        .json(&json!({}))
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
        .post(url(&ts, "/api/auth/tokens/list"))
        .bearer_auth(&user_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0]["name"], "alice-api");

    // Operation dispatch deserializes the body before the actor check runs, so
    // an empty `{}` would 400 on any operation with a required field before
    // ever reaching the 403 this loop checks for — each entry below carries
    // the minimal body its operation needs.
    let forbidden = [
        (reqwest::Method::POST, "/api/settings/patch", json!({})),
        (
            reqwest::Method::POST,
            "/api/deployment/reconcile",
            json!({}),
        ),
        (reqwest::Method::POST, "/api/auth/users/list", json!({})),
        (
            reqwest::Method::POST,
            "/api/auth/github_config/get",
            json!({}),
        ),
        (
            reqwest::Method::POST,
            "/api/auth/automation_token",
            json!({ "subject": "probe" }),
        ),
        (
            reqwest::Method::POST,
            "/api/auth/federations/list",
            json!({}),
        ),
        (
            reqwest::Method::POST,
            "/api/profiles/create",
            json!({
                "name": "probe",
                "agent_kind": "shell",
                "ambient_allowlist": [],
                "github_repositories": [],
                "runtime_permissions": []
            }),
        ),
        (
            reqwest::Method::POST,
            "/api/agents/custom/create",
            json!({ "name": "probe" }),
        ),
        (
            reqwest::Method::POST,
            "/api/mcps/custom/create",
            json!({ "identity": "/probe", "label": "probe", "source": "# probe" }),
        ),
        (reqwest::Method::GET, "/api/shell/terminal", json!({})),
        (reqwest::Method::POST, "/api/shell/restart", json!({})),
        (
            reqwest::Method::POST,
            "/api/watches/create",
            json!({ "name": "probe" }),
        ),
        (
            reqwest::Method::POST,
            "/api/watches/run",
            json!({ "key": "status" }),
        ),
    ];
    for (method, path, body) in forbidden {
        let response = http
            .request(method, url(&ts, path))
            .bearer_auth(&user_token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "user request {path}"
        );
    }

    // `settings.env.set` takes the variable name in the body rather than the
    // path, so unlike the bare entries above it needs a real payload to clear
    // input validation before the actor check gets a chance to reject it —
    // the same reason `/api/settings/get` was lifted into the `(path, body)`
    // reachable list earlier in this test.
    let response = http
        .post(url(&ts, "/api/settings/env/set"))
        .bearer_auth(&user_token)
        .json(&json!({ "name": "SHARED_VALUE", "value": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "user request /api/settings/env/set"
    );

    let promoted: Value = http
        .post(url(&ts, "/api/auth/users/set_role"))
        .bearer_auth(&admin_token)
        .json(&json!({ "username": "alice", "role": "admin" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(promoted["role"], "admin");
    let response = http
        .post(url(&ts, "/api/settings/patch"))
        .bearer_auth(&user_token)
        .json(&json!({ "changes": { "terminal.theme": "light" } }))
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
        .post(url(&ts, "/api/auth/tokens/create"))
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
    http.post(url(&ts, "/api/settings/patch"))
        .json(&json!({ "changes": { "auth.trust_loopback": false } }))
        .send()
        .await
        .unwrap();

    // Health stays public (liveness probes must not need a token).
    let r = http.get(url(&ts, "/api/health")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // `auth.me` is declared `actor = Anonymous`, so it answers without a
    // credential and reports the caller as unauthenticated.
    let me: Value = http
        .post(url(&ts, "/api/auth/me"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["authenticated"], false);

    let r = http
        .post(url(&ts, "/api/branches/list"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn registered_and_custom_api_routes_are_both_protected() {
    let ts = TestServer::start().await;
    ts.client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "auth.trust_loopback": false } }),
        )
        .await
        .unwrap();
    let http = reqwest::Client::new();

    for (method, path) in [
        (reqwest::Method::POST, "/api/issues/list"),
        (reqwest::Method::POST, "/api/sessions/list"),
    ] {
        let response = http
            .request(method.clone(), url(&ts, path))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} was not mounted behind auth",
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
            "/api/sessions/launch",
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
            "/api/sessions/launch",
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
        .post(
            "/api/settings/patch",
            json!({ "changes": { "auth.trust_loopback": false } }),
        )
        .await
        .unwrap();
    let http = reqwest::Client::new();

    let own = http
        .post(url(&ts, "/api/sessions/get"))
        .bearer_auth(&token)
        .json(&json!({ "session": session_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    let own_channel = http
        .post(url(&ts, "/api/channels/get"))
        .bearer_auth(&token)
        .json(&json!({ "channel": session_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(own_channel.status(), StatusCode::OK);
    let issue = http
        .post(url(&ts, "/api/issues/get"))
        .bearer_auth(&token)
        .json(&json!({ "id": tracking_issue }))
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), StatusCode::OK);
    let own_history = http
        .post(url(&ts, "/api/sessions/history/list"))
        .bearer_auth(&token)
        .json(&json!({ "session": session_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(own_history.status(), StatusCode::OK);

    let child = http
        .post(url(&ts, "/api/sessions/launch"))
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
        .post(url(&ts, "/api/channels/get"))
        .bearer_auth(&token)
        .json(&json!({ "channel": child_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(child_channel.status(), StatusCode::OK);
    let custom = http
        .post(url(&ts, "/api/channels/create"))
        .bearer_auth(&token)
        .json(&json!({ "name": "shared review", "topic": "explicit pipe" }))
        .send()
        .await
        .unwrap();
    assert_eq!(custom.status(), StatusCode::OK);
    let custom: Value = custom.json().await.unwrap();
    let custom_id = custom["id"].as_str().unwrap();
    let invite = http
        .post(url(&ts, "/api/channels/subscription/set"))
        .bearer_auth(&token)
        .json(&json!({ "channel": custom_id, "mode": "deliver", "session": child_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(invite.status(), StatusCode::OK);
    let invite: Value = invite.json().await.unwrap();
    assert_eq!(invite["subject_id"], child_id);
    assert_eq!(invite["mode"], "deliver");
    let visible_channels = http
        .post(url(&ts, "/api/channels/list"))
        .bearer_auth(&token)
        .json(&json!({}))
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
        .post(url(&ts, "/api/channels/archive"))
        .bearer_auth(&child_token)
        .json(&json!({ "channel": custom_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(invited_archive.status(), StatusCode::FORBIDDEN);
    let sibling_channel = http
        .post(url(&ts, "/api/channels/get"))
        .bearer_auth(&token)
        .json(&json!({ "channel": sibling_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(sibling_channel.status(), StatusCode::FORBIDDEN);
    let creator_archive = http
        .post(url(&ts, "/api/channels/archive"))
        .bearer_auth(&token)
        .json(&json!({ "channel": custom_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(creator_archive.status(), StatusCode::OK);

    let unrelated_response = http
        .post(url(&ts, "/api/issues/get"))
        .bearer_auth(&token)
        .json(&json!({ "id": unrelated.id }))
        .send()
        .await
        .unwrap();
    assert_eq!(unrelated_response.status(), StatusCode::OK);
    let close_unrelated = http
        .post(url(&ts, "/api/issues/close"))
        .bearer_auth(&token)
        .json(&json!({ "ids": [unrelated.id] }))
        .send()
        .await
        .unwrap();
    assert_eq!(close_unrelated.status(), StatusCode::OK);
    let foreign_response = http
        .post(url(&ts, "/api/issues/get"))
        .bearer_auth(&token)
        .json(&json!({ "id": foreign.id }))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_response.status(), StatusCode::FORBIDDEN);
    let close_foreign = http
        .post(url(&ts, "/api/issues/close"))
        .bearer_auth(&token)
        .json(&json!({ "ids": [foreign.id] }))
        .send()
        .await
        .unwrap();
    assert_eq!(close_foreign.status(), StatusCode::FORBIDDEN);
    for (path, body) in [
        (
            "/api/sessions/history/list",
            json!({ "session": sibling_id }),
        ),
        (
            "/api/sessions/history/search",
            json!({ "session": sibling_id, "q": "secret" }),
        ),
    ] {
        let sibling_history = http
            .post(url(&ts, path))
            .bearer_auth(&token)
            .json(&body)
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
        .post(url(&ts, "/api/auth/tokens/list"))
        .bearer_auth(&token)
        .json(&json!({}))
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
            .post(url(&ts, "/api/session_layout/get"))
            .bearer_auth(credential)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(layout.status(), StatusCode::FORBIDDEN);
        let mutation = http
            .post(url(&ts, "/api/session_layout/move"))
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
            "/api/sessions/launch",
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
        .post(
            "/api/settings/patch",
            json!({ "changes": { "auth.trust_loopback": false } }),
        )
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
        .env("LOOM_TOKEN", &token)
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
        "branch ancestry is repository-scoped"
    );

    // The handler and its declared Session scope accept the same key forms.
    // In particular, a cross-repository child can be addressed by
    // `repo:branch`; authorization resolves that spelling before checking the
    // session tree instead of rejecting it as though it were a literal id.
    let child_branch = weaver_core::branch::get(&ts.state.db, &child.branch_id)
        .await
        .unwrap()
        .unwrap();
    let child_key = format!("{}:{}", child_branch.repo_root, child_branch.branch);
    let response = reqwest::Client::new()
        .post(url(&ts, "/api/sessions/get"))
        .bearer_auth(&token)
        .json(&json!({ "session": child_key }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let resolved: serde_json::Value = response.json().await.unwrap();
    assert_eq!(resolved["id"], child.id);
}

/// A branch key is not a branch. Whatever a session credential spells, the
/// branch it reaches is the one the key *resolves* to, and that row has to be
/// in its own tree.
#[tokio::test]
#[serial]
async fn session_token_is_refused_a_foreign_branch_in_every_key_form() {
    let ts = TestServer::start().await;
    let created = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({ "cwd": ts.cwd(), "goal": "branch scope", "agent": "shell" }),
        )
        .await
        .unwrap();
    let session_id = created["id"].as_str().unwrap();
    let branch_id = created["branch"]["id"].as_str().unwrap().to_string();
    let branch_name = created["branch"]["branch"].as_str().unwrap().to_string();
    let token =
        loom::auth::create_session_token(&ts.state.db, Some("rjpower"), session_id, &branch_id)
            .await
            .unwrap();

    // Same branch name, different repository, inserted afterwards so it wins
    // `resolve_key`'s newest-first tiebreak. The id is fixed so the test can
    // name a prefix of it.
    let foreign_repo = format!("{}-foreign", ts.cwd());
    let foreign = weaver_core::branch::insert(
        &ts.state.db,
        "zzzzfore",
        &foreign_repo,
        &branch_name,
        "main",
    )
    .await
    .unwrap();

    let http = reqwest::Client::new();
    let get_branch = |key: String| {
        let http = http.clone();
        let url = url(&ts, "/api/branches/get");
        let token = token.clone();
        async move {
            http.post(url)
                .bearer_auth(&token)
                .json(&json!({ "branch": key }))
                .send()
                .await
                .unwrap()
        }
    };

    for key in [
        foreign.id.clone(),
        "zzzz".to_string(),
        format!("{foreign_repo}:{branch_name}"),
        // The bare name is ambiguous and resolves to the newest match — the
        // foreign row; denying is the fail-closed answer.
        branch_name.clone(),
        // `LIKE` metacharacters resolve literally, not as SQL wildcards.
        "%".to_string(),
        "_".to_string(),
    ] {
        let response = get_branch(key.clone()).await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "branch key {key:?} should not reach outside the session's tree"
        );
    }

    // The forms that name the caller's own branch still work — `repo_root:name`,
    // and the empty key, which is how an agent says "my branch" by saying nothing.
    for key in [
        branch_id.clone(),
        format!("{}:{branch_name}", ts.cwd()),
        String::new(),
    ] {
        let response = get_branch(key.clone()).await;
        assert_eq!(response.status(), StatusCode::OK, "own branch as {key:?}");
        let view: Value = response.json().await.unwrap();
        assert_eq!(view["id"].as_str(), Some(branch_id.as_str()));
    }
}
