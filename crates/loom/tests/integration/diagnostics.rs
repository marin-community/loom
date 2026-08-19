//! Operational endpoints: public liveness/readiness/metrics and the human-readable,
//! redacted diagnostics inventory.

use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;

use super::fixtures::TestServer;

fn url(ts: &TestServer, path: &str) -> String {
    format!("http://{}{}", ts.addr, path)
}

async fn seed_operational_state(ts: &TestServer) {
    sqlx::query(
        "INSERT INTO branches (id, repo_root, branch, updated_at)
         VALUES ('diag-branch', '/sensitive/repository/path',
                 'weaver/sensitive-branch-name', '2026-07-22T10:00:00.000Z')",
    )
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE profiles SET max_concurrent = 2, revision = 7 WHERE name = 'default'")
        .execute(&ts.state.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sessions
         (id, branch_id, work_dir, term_session, agent_kind, status,
          last_activity_at, class, protocol, profile, creator_subject)
         VALUES ('sensitive-session-id', 'diag-branch', '/sensitive/worktree/path',
                 'sensitive-terminal-id', 'shell', 'orphaned',
                 '2026-07-22T10:01:00.000Z', 'automation', 'acp', 'default',
                 'sensitive-user-name')",
    )
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO automation_runs
         (id, actor_subject, source, service_tag, profile, idempotency_key, request_json,
          session_id, status, outcome, summary, created_at, updated_at)
         VALUES ('sensitive-run-id', 'sensitive-actor', 'actions', 'weaver-actions', 'default',
                 'sensitive-token-id', '{\"secret\":\"do-not-return\"}',
                 'sensitive-session-id', 'failed', 'secret-outcome-text',
                 'raw failure contains bearer-secret',
                 '2026-07-22T09:00:00.000Z', '2026-07-22T10:02:00.000Z')",
    )
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO federation_mappings
         (id, name, provider, issuer, audience, subject, service_account,
          service_tag, profiles_json, created_at, updated_at)
         VALUES ('mapping-internal-id', 'ops-service', 'google',
                 'https://accounts.google.com', 'loom-production', '12345',
                 'ops@example.iam.gserviceaccount.com', 'ops', '[\"default\"]',
                 '2026-07-22T09:00:00.000Z', '2026-07-22T09:00:00.000Z')",
    )
    .execute(&ts.state.db)
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn diagnostics_are_correct_redacted_and_human_only() {
    let ts = TestServer::start().await;
    seed_operational_state(&ts).await;
    let http = reqwest::Client::new();

    let typed = ts.client.diagnostics().await.unwrap();
    assert_eq!(typed.profiles[0].active, 1);
    assert_eq!(typed.automation_runs.recent_failures.len(), 1);

    let response = http.get(url(&ts, "/api/diagnostics")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let diagnostics: Value = response.json().await.unwrap();
    let orphan = diagnostics["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["status"] == "orphaned")
        .unwrap();
    assert_eq!(orphan["count"], 1);
    assert_eq!(orphan["class"], "automation");
    assert_eq!(orphan["profile"], "default");
    assert_eq!(orphan["protocol"], "acp");
    assert_eq!(orphan["runner_pool"], "local");
    assert_eq!(diagnostics["profiles"][0]["active"], 1);
    assert_eq!(diagnostics["profiles"][0]["maximum"], 2);
    assert_eq!(diagnostics["profiles"][0]["available"], 1);
    assert_eq!(
        diagnostics["automation_runs"]["recent_failures"][0]["profile"],
        "default"
    );
    assert_eq!(
        diagnostics["automation_runs"]["counts"][0]["service_tag"],
        "weaver-actions"
    );
    assert_eq!(
        diagnostics["automation_runs"]["recent_failures"][0]["outcome"],
        "other"
    );
    assert_eq!(diagnostics["problems"][0]["status"], "orphaned");
    assert_eq!(diagnostics["federations"][0]["name"], "ops-service");
    assert_eq!(diagnostics["federations"][0]["service_tag"], "ops");
    assert_eq!(diagnostics["migrations"][0]["ready"], true);
    assert_eq!(diagnostics["migrations"][1]["ready"], true);

    let encoded = diagnostics.to_string();
    for forbidden in [
        "sensitive-session-id",
        "sensitive-branch-name",
        "/sensitive/repository/path",
        "/sensitive/worktree/path",
        "sensitive-terminal-id",
        "sensitive-user-name",
        "sensitive-actor",
        "sensitive-token-id",
        "do-not-return",
        "bearer-secret",
        "secret-outcome-text",
        "mapping-internal-id",
        "12345",
        "ops@example.iam.gserviceaccount.com",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "diagnostics leaked {forbidden}"
        );
    }

    let session_token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        "sensitive-session-id",
        "diag-branch",
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

    let unauthenticated = http.get(url(&ts, "/api/diagnostics")).send().await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let scoped = http
        .get(url(&ts, "/api/diagnostics"))
        .bearer_auth(session_token)
        .send()
        .await
        .unwrap();
    assert_eq!(scoped.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial]
async fn health_readiness_and_metrics_are_public_and_label_safe() {
    let ts = TestServer::start().await;
    seed_operational_state(&ts).await;
    ts.client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "auth.trust_loopback": false } }),
        )
        .await
        .unwrap();
    let http = reqwest::Client::new();

    for path in ["/api/health", "/api/health/live"] {
        let response = http.get(url(&ts, path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.text().await.unwrap(), "ok");
    }
    let ready = http.get(url(&ts, "/api/ready")).send().await.unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let ready: Value = ready.json().await.unwrap();
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["database"], true);
    assert_eq!(ready["degraded"], json!([]));
    assert_eq!(ts.client.readiness().await.unwrap().status, "ready");

    let metrics = http.get(url(&ts, "/metrics")).send().await.unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    assert_eq!(
        metrics.headers()["content-type"],
        "application/openmetrics-text; version=1.0.0; charset=utf-8"
    );
    let metrics = metrics.text().await.unwrap();
    assert!(metrics.contains("loom_sessions_current{"));
    assert!(metrics.contains("status=\"orphaned\""));
    assert!(metrics.contains("profile=\"default\""));
    assert!(metrics.contains("runner_pool=\"local\""));
    assert!(metrics.contains("loom_profile_capacity{profile=\"default\",state=\"available\"} 1"));
    assert!(metrics.contains("loom_automation_runs_current{"));
    assert!(!metrics.contains("loom_automation_runs_total"));
    assert!(metrics.contains("loom_migration_ready{stream=\"loom\"} 1"));
    assert!(metrics.ends_with("# EOF\n"));
    for forbidden in [
        "sensitive-session-id",
        "sensitive-branch-name",
        "/sensitive/repository/path",
        "/sensitive/worktree/path",
        "sensitive-user-name",
        "sensitive-token-id",
        "bearer-secret",
    ] {
        assert!(!metrics.contains(forbidden), "metrics leaked {forbidden}");
    }
}

#[tokio::test]
#[serial]
async fn readiness_fails_when_a_migration_stream_is_incomplete() {
    let ts = TestServer::start().await;
    let latest = loom::db::latest_migration_version();
    sqlx::query("DELETE FROM loom_schema_migrations WHERE version = ?")
        .bind(latest)
        .execute(&ts.state.db)
        .await
        .unwrap();

    let response = reqwest::Client::new()
        .get(url(&ts, "/api/ready"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["database"], true);
    assert_eq!(body["migrations"][1]["ready"], false);
    assert_eq!(body["migrations"][1]["expected"], latest);
}
