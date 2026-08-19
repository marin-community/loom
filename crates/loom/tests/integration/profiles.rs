//! Profiles are the sole launch-policy and agent-environment authority.

use reqwest::StatusCode;
use serde_json::json;
use serial_test::serial;
use std::process::Command;

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
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

fn interactive_shell_profile(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "description": "launch test",
        "agent_kind": "shell",
        "model": "",
        "effort": "",
        "protocol": "",
        "mode": "auto",
        "class": "interactive",
        "strict": false,
        "env_clear": false,
        "ambient_allowlist": [],
        "max_concurrent": 0,
        "prelude": "weaver",
        "restricted": false,
        "runtime_permissions": [],
        "mcp_access": { "mode": "none", "groups": [] }
    })
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profile_capacity_admission_is_serialized_across_repositories() {
    let ts = TestServer::start().await;
    let mut profile = interactive_shell_profile("one-at-a-time");
    profile["max_concurrent"] = json!(1);
    ts.client
        .post("/api/profiles/create", profile)
        .await
        .unwrap();

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

    let permit = ts.state.launch_gate.acquire_profile("one-at-a-time").await;
    let url = format!("http://{}/api/sessions/launch", ts.addr);
    let first_http = reqwest::Client::new();
    let first_url = url.clone();
    let first_cwd = ts.cwd();
    let first = tokio::spawn(async move {
        first_http
            .post(first_url)
            .json(&json!({
                "cwd": first_cwd,
                "goal": "first profile admission",
                "profile": "one-at-a-time"
            }))
            .send()
            .await
            .unwrap()
    });
    let second_http = reqwest::Client::new();
    let second_cwd = other_repo.path().to_string_lossy().into_owned();
    let second = tokio::spawn(async move {
        second_http
            .post(url)
            .json(&json!({
                "cwd": second_cwd,
                "goal": "second profile admission",
                "profile": "one-at-a-time"
            }))
            .send()
            .await
            .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !first.is_finished(),
        "first launch waits for profile admission"
    );
    assert!(
        !second.is_finished(),
        "a different repository still waits for the same profile"
    );
    drop(permit);

    let first = tokio::time::timeout(std::time::Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap();
    let statuses = [first.status(), second.status()];
    assert_eq!(
        statuses.iter().filter(|status| status.is_success()).count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );

    let (successful, conflict) = if first.status().is_success() {
        (first, second)
    } else {
        (second, first)
    };
    let conflict_body: serde_json::Value = conflict.json().await.unwrap();
    assert_eq!(conflict_body["preview"]["capacity"]["active"], 1);
    assert_eq!(conflict_body["preview"]["capacity"]["allowed"], false);
    assert_eq!(conflict_body["preview"]["valid"], false);
    let session: serde_json::Value = successful.json().await.unwrap();
    assert_eq!(
        loom::profile::active_count(&ts.state.db, "one-at-a-time")
            .await
            .unwrap(),
        1
    );
    ts.client
        .post(
            "/api/sessions/delete",
            json!({ "session": session["id"].as_str().unwrap() }),
        )
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_and_mcp_rest_journey() {
    let ts = TestServer::start_api_only().await;
    let stock = ts
        .client
        .post("/api/profiles/get", json!({ "name": "github_comment" }))
        .await
        .unwrap();
    assert_eq!(stock["restricted"], true);
    assert_eq!(stock["mcp_access"]["groups"], json!(["github"]));
    assert!(stock["env"].as_array().unwrap().is_empty());

    let registry = ts.client.post("/api/mcps/get", json!({})).await.unwrap();
    let github = registry["capability_sets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|set| set["name"] == "mcp/github/comment@v1")
        .unwrap();
    assert!(github["digest"].as_str().unwrap().starts_with("sha256:"));
    assert!(github.get("source").is_none());

    ts.client
        .post(
            "/api/profiles/create",
            interactive_shell_profile("clone-source"),
        )
        .await
        .unwrap();
    let source_profile = ts
        .client
        .post(
            "/api/profiles/env/set",
            json!({ "profile": "clone-source", "name": "TOKEN", "value": "write-only-source" }),
        )
        .await
        .unwrap();
    assert!(!source_profile.to_string().contains("write-only-source"));
    ts.client
        .post(
            "/api/profiles/env/set",
            json!({ "profile": "clone-source", "name": "REMOVE_ME", "value": "discarded" }),
        )
        .await
        .unwrap();
    let cli = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "profile",
            "clone",
            "clone-source",
            "cli-clone",
            "--copy-environment",
            "--remove-environment",
            "REMOVE_ME",
            "--set-environment",
            "TOKEN=from-cli",
            "--secret-environment",
            "SECRET=projects/acme/secrets/cli/versions/latest",
        ])
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "CLI clone failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cloned = ts
        .client
        .post("/api/profiles/get", json!({ "name": "cli-clone" }))
        .await
        .unwrap();
    assert_eq!(
        cloned["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["SECRET", "TOKEN"]
    );
    assert!(!cloned.to_string().contains("from-cli"));

    let source = r#"
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    if request["method"] == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "test-custom", "version": "1"},
        }
    elif request["method"] == "tools/list":
        result = {
            "tools": [{
                "name": "ping",
                "description": "Return a value.",
                "inputSchema": {"type": "object"},
            }]
        }
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
    let test_source = r#"
import os
from pathlib import Path

assert "tools/list" in Path(os.environ["LOOM_MCP_SOURCE"]).read_text()
print("custom tests passed")
"#;
    let custom = ts
        .client
        .post(
            "/api/mcps/custom/create",
            json!({
                "identity": "/ops/status",
                "label": "Status helper",
                "description": "Test custom MCP",
                "source": source,
                "test_source": test_source,
                "enabled": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(custom["group"], "ops");
    assert_eq!(custom["revision"], 1);
    assert_eq!(custom["validation_state"], "ready");
    assert_eq!(custom["tools"], json!(["ping"]));
    assert!(custom["validation_message"]
        .as_str()
        .unwrap()
        .contains("custom tests passed"));

    let registry = ts.client.post("/api/mcps/get", json!({})).await.unwrap();
    assert!(registry["custom_servers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|server| server["identity"] == "/ops/status"));

    let profile_req = json!({
        "name": "custom-tools",
        "description": "ordinary ACP profile with custom tools",
        "agent_kind": "claude",
        "protocol": "acp",
        "mode": "default",
        "mcp_access": {"mode": "groups", "groups": ["ops"]}
    });
    let created_profile = ts
        .client
        .post("/api/profiles/create", profile_req.clone())
        .await
        .unwrap();
    assert_eq!(created_profile["revision"], 1);
    let effective = ts
        .client
        .post("/api/profiles/effective", json!({ "name": "custom-tools" }))
        .await
        .unwrap();
    assert_eq!(
        effective["mcp_policy"]["custom_servers"][0]["identity"],
        "/ops/status"
    );
    assert_eq!(effective["mcp_policy"]["custom_servers"][0]["revision"], 1);
    assert!(effective["runtime_permissions"][0]
        .as_str()
        .unwrap()
        .starts_with("mcp__loom_custom_"));
    assert!(
        std::path::Path::new(effective["mcp_servers"][0]["command"].as_str().unwrap())
            .is_absolute()
    );
    assert_eq!(
        effective["mcp_servers"][0]["args"],
        json!(["mcp", "serve-custom", "/ops/status"])
    );
    let source_v2 = source.replace("Return a value.", "Return a pinned value.");
    let edited = ts
        .client
        .post(
            "/api/mcps/custom/update",
            json!({
                "identity": "/ops/status",
                "label": "Status helper",
                "description": "Test custom MCP",
                "source": source_v2.clone(),
                "test_source": test_source,
                "enabled": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(edited["revision"], 2);
    let still_pinned = ts
        .client
        .post("/api/profiles/effective", json!({ "name": "custom-tools" }))
        .await
        .unwrap();
    assert_eq!(
        still_pinned["mcp_policy"]["custom_servers"][0]["revision"],
        1
    );

    let reconciled = ts
        .client
        .post("/api/profiles/update", profile_req)
        .await
        .unwrap();
    assert_eq!(reconciled["revision"], 2);
    let effective = ts
        .client
        .post("/api/profiles/effective", json!({ "name": "custom-tools" }))
        .await
        .unwrap();
    assert_eq!(effective["mcp_policy"]["custom_servers"][0]["revision"], 2);

    let disabled = ts
        .client
        .post(
            "/api/mcps/custom/update",
            json!({
                "identity": "/ops/status",
                "label": "Status helper",
                "description": "Test custom MCP",
                "source": source_v2,
                "test_source": test_source,
                "enabled": false
            }),
        )
        .await
        .unwrap();
    assert_eq!(disabled["identity"], "/ops/status");
    assert_eq!(disabled["revision"], 3);

    let response = reqwest::Client::new()
        .post(format!("http://{}/api/mcps/custom/delete", ts.addr))
        .json(&json!({ "identity": "/ops/status" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("pinned by profile"));

    let response = reqwest::Client::new()
        .post(format!("http://{}/api/profiles/delete", ts.addr))
        .json(&json!({ "name": "custom-tools" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let deleted: serde_json::Value = response.json().await.unwrap();
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["name"], "custom-tools");

    // `profiles.delete` and `mcps.custom.delete` are ordinary operations and
    // reply 200 with the typed delete result.
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/mcps/custom/delete", ts.addr))
        .json(&json!({ "identity": "/ops/status" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let deleted: serde_json::Value = response.json().await.unwrap();
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["identity"], "/ops/status");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_github_profile_launch_wires_policy_prompt_and_server_api() {
    let _adapter = EnvVarGuard::set(
        "WEAVER_CLAUDE_ACP_CMD",
        &crate::fixtures::fake_acp_agent_cmd(),
    );
    let ts = TestServer::start_with_app().await;
    let goal = "say:ready";
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "cwd": ts.cwd(),
                "profile": "github_comment",
                "title": "Restricted GitHub tool test",
                "goal": goal
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        session["mcp_policy"]["capability_sets"][0]["name"],
        "loom/github/comment@v1"
    );
    assert!(session["mcp_policy"]["capability_sets"][0]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let id = session["id"].as_str().unwrap();
    let stored_session = loom::session::get(&ts.state.db, id).await.unwrap().unwrap();
    let session_token = loom::auth::create_session_token(
        &ts.state.db,
        stored_session.created_by.as_deref(),
        id,
        &stored_session.branch_id,
    )
    .await
    .unwrap();
    // `permissions.github.token` is the operation that serves this now. It is
    // declared `actor = SessionOnly` — no human may stand in for a session to
    // fetch its credential — and a *restricted* session is refused on top of
    // that, which is what this asserts.
    let token_response = reqwest::Client::new()
        .post(format!("http://{}/api/permissions/github/token", ts.addr))
        .bearer_auth(session_token)
        .json(&json!({ "session": id }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        token_response.status(),
        StatusCode::FORBIDDEN,
        "restricted agents must not retrieve the server-side credential"
    );
    assert!(ts
        .client
        .post(
            "/api/sessions/mode",
            json!({ "mode_id": "bypassPermissions", "session": id }),
        )
        .await
        .is_err());
    assert!(ts
        .client
        .post(
            "/api/sessions/handoff",
            json!({ "agent": "codex", "session": id }),
        )
        .await
        .is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let chat = ts
            .client
            .post("/api/sessions/chat", json!({ "session": id }))
            .await
            .unwrap();
        if let Some(message) = chat["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["kind"] == "user_message")
        {
            assert_eq!(message["payload"]["text"], goal);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "caller goal was never dispatched"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let stamped: String =
        sqlx::query_scalar("SELECT policy_allowed_tools FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    let stamped: Vec<String> = serde_json::from_str(&stamped).unwrap();
    assert!(stamped.contains(&"mcp__loom_github__issue_edit".to_string()));
    assert!(!stamped.contains(&"mcp/github/comment".to_string()));
    let mcp_policy: String =
        sqlx::query_scalar("SELECT policy_mcp_access FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert!(mcp_policy.contains("loom/github/comment@v1"));
    assert!(mcp_policy.contains("sha256:"));
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
            "/api/permissions/github/restricted/invoke",
            json!({ "session": id, "tool": "issue_edit", "arguments": { "number": 7, "body": "clean body" } }),
        )
        .await
        .unwrap();
    let text = response["text"].as_str().unwrap();
    assert!(text.contains("GitHub issue_edit completed for octo/fixed#7"));

    let second_response = ts
        .client
        .post(
            "/api/permissions/github/restricted/invoke",
            json!({ "session": id, "tool": "issue_view", "arguments": { "number": 7 } }),
        )
        .await
        .unwrap();
    let viewed: serde_json::Value =
        serde_json::from_str(second_response["text"].as_str().unwrap()).unwrap();
    assert_eq!(viewed["number"], 7);
    assert_eq!(viewed["title"], "issue 7 of octo/fixed");
    assert!(ts
        .client
        .post(
            "/api/permissions/github/restricted/invoke",
            json!({ "session": id, "tool": "issue_edit", "arguments": { "number": 8, "body": "wrong issue" } }),
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
            "/api/permissions/github/restricted/invoke",
            json!({ "session": id, "tool": "issue_edit", "arguments": { "number": 7, "body": "no longer allowed" } }),
        )
        .await
        .is_err());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automation_channel_reuses_one_acp_session_without_replaying_deliveries() {
    let _adapter = EnvVarGuard::set(
        "WEAVER_CLAUDE_ACP_CMD",
        &crate::fixtures::fake_acp_agent_cmd(),
    );
    let ts = TestServer::start().await;
    ts.client
        .post(
            "/api/profiles/create",
            json!({
                "name": "ops",
                "description": "operations intake",
                "agent_kind": "claude",
                "protocol": "acp",
                "mode": "default",
                "class": "automation",
                "strict": true,
                "env_clear": true,
                "max_concurrent": 1,
                "turn_budget": 20,
                "prelude": "none"
            }),
        )
        .await
        .unwrap();
    let first_request = json!({
        "profile": "ops",
        "source": "grafana",
        "channel": "operator",
        "idempotency_key": "alert:first",
        "session": {
            "cwd": ts.cwd(),
            "title": "Grafana operator",
            "goal": "first alert"
        }
    });
    let first = ts
        .client
        .post("/api/runs/create", first_request.clone())
        .await
        .unwrap();
    let second_request = json!({
        "profile": "ops",
        "source": "grafana",
        "channel": "operator",
        "idempotency_key": "alert:second",
        "session": {
            "cwd": ts.cwd(),
            "title": "Grafana operator",
            "goal": "second alert"
        }
    });
    let second = ts
        .client
        .post("/api/runs/create", second_request.clone())
        .await
        .unwrap();
    let duplicate = ts
        .client
        .post("/api/runs/create", second_request)
        .await
        .unwrap();
    let mut collision_request = first_request;
    collision_request["channel"] = json!("another-operator");
    collision_request["session"]["goal"] = json!("must not be delivered");
    let collision = ts
        .client
        .post("/api/runs/create", collision_request)
        .await
        .unwrap();

    assert_ne!(first["id"], second["id"]);
    assert_eq!(first["session_id"], second["session_id"]);
    assert_eq!(second["id"], duplicate["id"]);
    assert_eq!(second["channel"], "operator");
    assert_eq!(collision["id"], first["id"]);
    assert_eq!(collision["channel"], "operator");

    let sessions = ts
        .client
        .post("/api/sessions/summary/list", json!({ "automation": true }))
        .await
        .unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    let launched = ts
        .client
        .post(
            "/api/sessions/get",
            json!({ "session": first["session_id"].as_str().unwrap() }),
        )
        .await
        .unwrap();
    assert_eq!(launched["origin"], "grafana");
    assert_eq!(launched["placement"]["space_name"], "Ops");
    assert_eq!(launched["placement"]["group_name"], "Inbox");

    let chat = ts
        .client
        .post(
            "/api/sessions/chat",
            json!({ "session": first["session_id"].as_str().unwrap() }),
        )
        .await
        .unwrap();
    let second_deliveries = chat["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| {
            block["kind"] == "user_message" && block["payload"]["text"] == "second alert"
        })
        .count();
    assert_eq!(second_deliveries, 1);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deployment_reconcile_rest_journey() {
    let ts = TestServer::start_api_only().await;
    let invalid = ts
        .client
        .post(
            "/api/deployment/reconcile",
            json!({
                "settings": { "slack.status_updates": "sometimes" },
                "profiles": [],
                "federations": []
            }),
        )
        .await
        .unwrap_err();
    assert!(
        invalid.to_string().contains("slack.status_updates"),
        "validation error should name the setting: {invalid}"
    );

    let manifest = json!({
        "settings": {
            "slack.status_updates": false,
            "slack.prompt_instructions": "Use the Marin response style."
        },
        "profiles": [{
            "profile": {
                "name": "ops",
                "description": "operations automation",
                "agent_kind": "claude",
                "protocol": "acp",
                "mode": "plan",
                "class": "automation",
                "strict": true,
                "env_clear": true,
                "max_concurrent": 1,
                "turn_budget": 20,
                "instructions": "Follow the deployment-owned incident workflow.",
                "mcp_access": {"mode": "groups", "groups": ["messaging"]}
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
    let deployed_settings = first["settings"].as_array().unwrap();
    let prompt_setting = deployed_settings
        .iter()
        .find(|setting| setting["key"] == "slack.prompt_instructions")
        .unwrap();
    assert_eq!(prompt_setting["source"], "deployment");
    assert_eq!(
        prompt_setting["deployment_value"],
        "Use the Marin response style."
    );
    let status_setting = deployed_settings
        .iter()
        .find(|setting| setting["key"] == "slack.status_updates")
        .unwrap();
    assert_eq!(status_setting["value"], "false");
    assert_eq!(first["profiles"][0]["revision"], 2);
    assert_eq!(
        first["profiles"][0]["instructions"],
        "Follow the deployment-owned incident workflow."
    );
    assert_eq!(
        first["profiles"][0]["mcp_access"],
        json!({"mode": "groups", "groups": ["messaging"]})
    );
    assert_eq!(first["profiles"][0]["env"][0]["source"], "gcp_secret");
    assert_eq!(
        first["profiles"][0]["env"][0]["secret_ref"],
        "projects/example/secrets/ops-kubeconfig/versions/latest"
    );
    assert!(first["profiles"][0]["env"][0].get("value").is_none());
    let mapping_id = first["federations"][0]["id"].clone();
    assert_eq!(first["federations"][0]["service_tag"], "marin-ops");

    let runtime = ts
        .client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "slack.status_updates": true } }),
        )
        .await
        .unwrap();
    let runtime_setting = runtime["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|setting| setting["key"] == "slack.status_updates")
        .unwrap();
    assert_eq!(runtime_setting["value"], "true");
    assert_eq!(runtime_setting["source"], "runtime");
    assert_eq!(runtime_setting["deployment_value"], "false");

    let inherited = ts
        .client
        .post(
            "/api/settings/patch",
            json!({ "changes": { "slack.status_updates": serde_json::Value::Null } }),
        )
        .await
        .unwrap();
    let inherited_setting = inherited["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|setting| setting["key"] == "slack.status_updates")
        .unwrap();
    assert_eq!(inherited_setting["value"], "false");
    assert_eq!(inherited_setting["source"], "deployment");

    let second = ts
        .client
        .post("/api/deployment/reconcile", manifest)
        .await
        .unwrap();
    assert_eq!(second["profiles"][0]["revision"], 2);
    assert_eq!(second["federations"][0]["id"], mapping_id);

    ts.client
        .post(
            "/api/deployment/reconcile",
            json!({ "settings": {}, "profiles": [], "federations": [], "prune": true }),
        )
        .await
        .unwrap();
    let pruned = ts
        .client
        .post("/api/settings/get", json!({}))
        .await
        .unwrap();
    let pruned_setting = pruned["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|setting| setting["key"] == "slack.status_updates")
        .unwrap();
    assert_eq!(pruned_setting["value"], "true");
    assert_eq!(pruned_setting["source"], "default");
    let profile = reqwest::Client::new()
        .post(format!("http://{}/api/profiles/get", ts.addr))
        .json(&json!({ "name": "ops" }))
        .send()
        .await
        .unwrap();
    assert_eq!(profile.status(), StatusCode::NOT_FOUND);

    let resolver = ts.state.launch_gate.acquire_resolver().await;
    let reconcile_url = format!("http://{}/api/deployment/reconcile", ts.addr);
    let reconcile = tokio::spawn(async move {
        reqwest::Client::new()
            .post(reconcile_url)
            .json(&json!({
                "profiles": [{
                    "profile": {
                        "name": "deployment-registry-barrier",
                        "agent_kind": "shell",
                        "protocol": "terminal",
                        "mode": "auto",
                        "class": "interactive",
                        "mcp_access": { "mode": "none", "groups": [] }
                    },
                    "env": []
                }],
                "federations": [],
                "prune": false
            }))
            .send()
            .await
            .unwrap()
    });
    let mutation_url = format!("http://{}/api/agents/custom/update", ts.addr);
    let mutation = tokio::spawn(async move {
        reqwest::Client::new()
            .post(mutation_url)
            .json(&json!({
                "name": "shell",
                "label": "Shell changed to ACP",
                "setup": "",
                "launch": "node fake-acp.mjs",
                "resume": "",
                "reports_status": false,
                "protocol": "acp"
            }))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!reconcile.is_finished());
    assert!(!mutation.is_finished());
    drop(resolver);

    let reconciled = tokio::time::timeout(std::time::Duration::from_secs(10), reconcile)
        .await
        .unwrap()
        .unwrap();
    let mutated = tokio::time::timeout(std::time::Duration::from_secs(10), mutation)
        .await
        .unwrap()
        .unwrap();
    assert!(mutated.status().is_success());
    if reconciled.status().is_success() {
        let profile = loom::profile::get(&ts.state.db, "deployment-registry-barrier")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(profile.protocol, "terminal");
    } else {
        assert_eq!(reconciled.status(), StatusCode::BAD_REQUEST);
        assert!(
            loom::profile::get(&ts.state.db, "deployment-registry-barrier")
                .await
                .unwrap()
                .is_none(),
            "validation from the newer registry generation must not partly persist"
        );
    }
}
