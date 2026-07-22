//! Profiles are the sole launch-policy and agent-environment authority.

use reqwest::StatusCode;
use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_profile_crud_withholds_secrets_and_stamps_sessions() {
    let ts = TestServer::start().await;
    let profile = ts
        .client
        .post(
            "/api/profiles",
            json!({
                "name": "actions",
                "description": "restricted automation",
                "agent_kind": "shell",
                "protocol": "terminal",
                "mode": "auto",
                "class": "automation",
                "strict": true,
                "env_clear": true,
                "ambient_allowlist": ["LANG"],
                "max_concurrent": 1,
                "turn_budget": 10,
                "idle_archive_secs": 60
            }),
        )
        .await
        .unwrap();
    assert_eq!(profile["revision"], 1);

    let profile = ts
        .client
        .put(
            "/api/profiles/actions/env/SECRET_TOKEN",
            json!({ "value": "must-not-round-trip" }),
        )
        .await
        .unwrap();
    assert_eq!(profile["env"][0]["name"], "SECRET_TOKEN");
    assert!(
        !profile.to_string().contains("must-not-round-trip"),
        "profile responses must never expose secret values"
    );

    let error = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "cwd": ts.cwd(),
                "goal": "override forbidden",
                "profile": "actions",
                "agent": "shell"
            }),
        )
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not allow launch overrides"));

    let session = ts
        .client
        .post(
            "/api/sessions",
            json!({ "cwd": ts.cwd(), "goal": "profile launch", "profile": "actions" }),
        )
        .await
        .unwrap();
    assert_eq!(session["profile"], "actions");
    assert_eq!(session["profile_revision"], 1);
    assert_eq!(session["class"], "automation");

    let delete = reqwest::Client::new()
        .delete(format!("http://{}/api/profiles/actions", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::BAD_REQUEST);
}
