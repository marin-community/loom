//! Profiles are the sole launch-policy and agent-environment authority.

use reqwest::StatusCode;
use serde_json::json;
use serial_test::serial;
use std::os::unix::fs::PermissionsExt;

use crate::fixtures::TestServer;

struct EnvVarGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    fn unset(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stock_github_comment_profile_round_trips_restricted_policy() {
    let ts = TestServer::start().await;
    let profile = ts.client.get("/api/profiles/github_comment").await.unwrap();

    assert_eq!(profile["agent_kind"], "claude");
    assert_eq!(profile["protocol"], "acp");
    assert_eq!(profile["mode"], "default");
    assert_eq!(profile["prelude"], "none");
    assert_eq!(profile["restricted"], true);
    assert!(profile["allowed_tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| rule == "mcp/github/comment"));
    assert!(profile["env"].as_array().unwrap().is_empty());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_profile_sends_the_caller_goal_as_the_first_prompt() {
    let _adapter = EnvVarGuard::set(
        "WEAVER_CLAUDE_ACP_CMD",
        &crate::fixtures::fake_acp_agent_cmd(),
    );
    let ts = TestServer::start().await;
    ts.client
        .put(
            "/api/profiles/github_comment/env/GH_TOKEN",
            json!({ "value": "github-actions-token" }),
        )
        .await
        .unwrap();

    let goal = "say:caller supplied prompt";
    let session = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "cwd": ts.cwd(),
                "profile": "github_comment",
                "title": "Restricted prompt test",
                "goal": goal
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    assert!(ts
        .client
        .put(
            &format!("/api/sessions/{id}/mode"),
            json!({ "mode_id": "bypassPermissions" }),
        )
        .await
        .is_err());
    assert!(ts
        .client
        .post(
            &format!("/api/sessions/{id}/handoff"),
            json!({ "agent": "codex" }),
        )
        .await
        .is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let chat = ts
            .client
            .get(&format!("/api/sessions/{id}/chat"))
            .await
            .unwrap();
        if let Some(message) = chat["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["kind"] == "user_message")
        {
            assert_eq!(message["payload"]["text"], goal);
            assert!(!message["payload"]["text"]
                .as_str()
                .unwrap()
                .contains("weaver summary"));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "caller goal was never dispatched"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_restricted_launch_does_not_treat_the_app_as_an_unscoped_credential() {
    let _token = EnvVarGuard::unset("GH_TOKEN");
    let ts = TestServer::start().await;
    weaver_core::config::apply(
        &ts.state.db,
        &[
            (
                loom::github_app::APP_ID_KEY.to_string(),
                Some("123456".to_string()),
            ),
            (
                loom::github_app::APP_PRIVATE_KEY_KEY.to_string(),
                Some("configured-for-preflight".to_string()),
            ),
        ],
    )
    .await
    .unwrap();

    let response = reqwest::Client::new()
        .post(format!("http://{}/api/sessions", ts.addr))
        .json(&json!({
            "cwd": ts.cwd(),
            "profile": "github_comment",
            "goal": "no repository installation target"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);
    assert!(ts
        .client
        .get("/api/sessions")
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_github_tool_uses_the_server_side_token_and_fixed_repo() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n\
         case \"$GH_TOKEN\" in\n\
           server-only-token) printf 'profile:' ;;\n\
           *) exit 17 ;;\n\
         esac\n\
         printf '%s' \"$*\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _path = EnvVarGuard::set("PATH", &path);
    let _adapter = EnvVarGuard::set(
        "WEAVER_CLAUDE_ACP_CMD",
        &crate::fixtures::fake_acp_agent_cmd(),
    );
    let ts = TestServer::start().await;
    ts.client
        .put(
            "/api/profiles/github_comment/env/GH_TOKEN",
            json!({ "value": "server-only-token" }),
        )
        .await
        .unwrap();
    loom::user_token::set(&ts.state.db, "rjpower", "requester-token")
        .await
        .unwrap();
    let session = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "cwd": ts.cwd(),
                "profile": "github_comment",
                "title": "Restricted GitHub tool test",
                "goal": "say:ready"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let stamped: String =
        sqlx::query_scalar("SELECT policy_allowed_tools FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    let stamped: Vec<String> = serde_json::from_str(&stamped).unwrap();
    assert!(stamped.contains(&"mcp__loom_github__issue_edit".to_string()));
    assert!(!stamped.contains(&"mcp/github/comment".to_string()));
    let tracking = weaver_core::issue::add(
        &ts.state.db,
        &weaver_core::issue::NewIssue {
            repo_root: ts.cwd(),
            github_repo: Some("octo/fixed".to_string()),
            github_issue: Some(7),
            title: "Restricted target".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE sessions SET github_repo = 'octo/fixed', tracking_issue_id = ? WHERE id = ?",
    )
    .bind(tracking.id)
    .bind(id)
    .execute(&ts.state.db)
    .await
    .unwrap();

    let response = ts
        .client
        .post(
            &format!("/api/sessions/{id}/restricted-github/issue_edit"),
            json!({ "arguments": { "number": 7, "body": "clean body" } }),
        )
        .await
        .unwrap();
    let text = response["text"].as_str().unwrap();
    assert!(text.contains("profile:issue edit 7 --repo octo/fixed --body clean body"));
    assert!(!text.contains("server-only-token"));
    let config_mode = std::fs::metadata(loom::db::run_dir(id).join("restricted-gh-config"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(config_mode, 0o700);

    let second_response = ts
        .client
        .post(
            &format!("/api/sessions/{id}/restricted-github/issue_view"),
            json!({ "arguments": { "number": 7 } }),
        )
        .await
        .unwrap();
    assert!(second_response["text"]
        .as_str()
        .unwrap()
        .contains("profile:issue view 7 --repo octo/fixed"));
    assert!(ts
        .client
        .post(
            &format!("/api/sessions/{id}/restricted-github/issue_edit"),
            json!({ "arguments": { "number": 8, "body": "wrong issue" } }),
        )
        .await
        .is_err());

    sqlx::query("UPDATE sessions SET policy_allowed_tools = '[\"Read(./**)\"]' WHERE id = ?")
        .bind(id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    assert!(ts
        .client
        .post(
            &format!("/api/sessions/{id}/restricted-github/issue_edit"),
            json!({ "arguments": { "number": 7, "body": "no longer allowed" } }),
        )
        .await
        .is_err());
}

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

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deployment_manifest_reconciles_profiles_secret_refs_and_workload_identity() {
    let ts = TestServer::start().await;
    let manifest = json!({
        "profiles": [{
            "profile": {
                "name": "ops",
                "description": "operations automation",
                "agent_kind": "shell",
                "protocol": "terminal",
                "mode": "plan",
                "class": "automation",
                "strict": true,
                "env_clear": true,
                "max_concurrent": 1,
                "turn_budget": 20
            },
            "env": [{
                "name": "KUBECONFIG",
                "secret_ref": "projects/example/secrets/ops-kubeconfig/versions/latest"
            }]
        }],
        "federations": [{
            "name": "marin-ops",
            "provider": "google",
            "issuer": "https://accounts.google.com",
            "audience": "https://loom.example.com",
            "subject": "11223344556677889900",
            "service_account": "loom-marin-ops@example.iam.gserviceaccount.com",
            "service_tag": "marin-ops",
            "profiles": ["ops"]
        }],
        "prune": true
    });

    let first = ts
        .client
        .post("/api/deployment/reconcile", manifest.clone())
        .await
        .unwrap();
    assert_eq!(first["profiles"][0]["revision"], 1);
    assert_eq!(first["profiles"][0]["env"][0]["source"], "gcp_secret");
    assert_eq!(
        first["profiles"][0]["env"][0]["secret_ref"],
        "projects/example/secrets/ops-kubeconfig/versions/latest"
    );
    assert!(!first.to_string().contains("value"));
    let mapping_id = first["federations"][0]["id"].clone();
    assert_eq!(first["federations"][0]["service_tag"], "marin-ops");

    let second = ts
        .client
        .post("/api/deployment/reconcile", manifest)
        .await
        .unwrap();
    assert_eq!(second["profiles"][0]["revision"], 1);
    assert_eq!(second["federations"][0]["id"], mapping_id);

    ts.client
        .post(
            "/api/deployment/reconcile",
            json!({ "profiles": [], "federations": [], "prune": true }),
        )
        .await
        .unwrap();
    let profile = reqwest::Client::new()
        .get(format!("http://{}/api/profiles/ops", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(profile.status(), StatusCode::NOT_FOUND);
}
