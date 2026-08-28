//! Automation CLI behavior against an isolated HTTP stub.
//!
//! These commands are registry-derived thin clients, so the useful journey is
//! the process boundary: a token arrives on stdin, the run credential travels
//! only as a bearer header, and the typed flags become the expected JSON body.

use std::io::Write as _;
use std::process::{Command, Stdio};

use axum::http::HeaderMap;
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const OIDC_TOKEN: &str = "header.claims.signature";
const AUTOMATION_TOKEN: &str = "loom_automation_secret";

async fn federate(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body, json!({ "token": OIDC_TOKEN }));
    Json(json!({
        "token": AUTOMATION_TOKEN,
        "expires_at": 1_800_000_000
    }))
}

async fn create_run(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {AUTOMATION_TOKEN}")
    );
    assert_eq!(body["profile"], "github_comment");
    assert_eq!(body["idempotency_key"], "prose-cleanup:issue:123:abc123");
    assert_eq!(body["source"], "actions");
    assert_eq!(body["session"]["repo"], "marin-community/marin");
    assert_eq!(body["session"]["title"], "Clean prose");
    assert_eq!(body["session"]["goal"], "Fix #123");
    assert_eq!(body["session"]["github_issue"], 123);

    Json(json!({
        "id": "run-1",
        "actor_subject": "github:123:workflow",
        "source": "actions",
        "watch_id": null,
        "service_tag": "github-actions",
        "profile": "github_comment",
        "idempotency_key": "github-caller:123:prose-cleanup:issue:123:abc123",
        "channel": null,
        "session_id": "session-1",
        "status": "launched",
        "outcome": null,
        "summary": "",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z"
    }))
}

fn cli(api: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
    command
        .env("WEAVER_API", api)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_actions_can_federate_and_create_an_idempotent_run() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/api/auth/federate", post(federate))
        .route("/api/runs/create", post(create_run));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut child = cli(&api)
        .args(["auth", "federate"])
        .env_remove("LOOM_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(OIDC_TOKEN.as_bytes())
        .unwrap();
    let federated = child.wait_with_output().unwrap();
    assert!(
        federated.status.success(),
        "federation failed: {}",
        String::from_utf8_lossy(&federated.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&federated.stdout).unwrap()["token"],
        AUTOMATION_TOKEN
    );
    assert!(!String::from_utf8_lossy(&federated.stderr).contains(OIDC_TOKEN));

    let session = json!({
        "repo": "marin-community/marin",
        "title": "Clean prose",
        "goal": "Fix #123",
        "github_issue": 123
    })
    .to_string();
    let created = cli(&api)
        .args([
            "runs",
            "create",
            "--profile",
            "github_comment",
            "--idempotency-key",
            "prose-cleanup:issue:123:abc123",
            "--session",
            &session,
        ])
        .env("LOOM_TOKEN", AUTOMATION_TOKEN)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "run creation failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&created.stdout).unwrap()["id"],
        "run-1"
    );
    assert!(!String::from_utf8_lossy(&created.stderr).contains(AUTOMATION_TOKEN));
}
