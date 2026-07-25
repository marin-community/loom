//! Profiles are the sole launch-policy and agent-environment authority.

use reqwest::StatusCode;
use serde_json::json;
use serial_test::serial;
use std::os::unix::fs::PermissionsExt;
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
async fn profile_environment_delete_returns_the_incremented_revision() {
    let ts = TestServer::start().await;
    let before = ts.client.get("/api/profiles/default").await.unwrap();
    let before_revision = before["revision"].as_i64().unwrap();

    let after_set = ts
        .client
        .put(
            "/api/profiles/default/env/API_TOKEN",
            json!({ "value": "write-only" }),
        )
        .await
        .unwrap();
    assert_eq!(after_set["revision"], before_revision + 1);

    let after_delete = ts
        .client
        .delete("/api/profiles/default/env/API_TOKEN")
        .await
        .unwrap();
    assert_eq!(after_delete["revision"], before_revision + 2);
    assert!(after_delete["env"].as_array().unwrap().is_empty());
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_resolution_reports_agent_defaults_and_rejects_environment_revision_drift() {
    let ts = TestServer::start().await;
    ts.client
        .post("/api/profiles", interactive_shell_profile("launch-test"))
        .await
        .unwrap();
    let preview = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({
                "selection": { "profile": "launch-test", "overrides": {} }
            }),
        )
        .await
        .unwrap();
    assert_eq!(preview["provenance"]["model"], "agent_default");
    assert_eq!(preview["provenance"]["effort"], "agent_default");
    // New profile writes normalize protocol; the resolver unit test covers
    // legacy empty protocol rows and must label those agent_default.
    assert_eq!(preview["provenance"]["protocol"], "profile");
    assert_eq!(preview["protocol"], "terminal");

    let unstamped = reqwest::Client::new()
        .post(format!("http://{}/api/sessions", ts.addr))
        .json(&json!({
            "cwd": ts.cwd(),
            "goal": "canonical launch needs its reviewed revisions",
            "selection": { "profile": "launch-test", "overrides": {} }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unstamped.status(), StatusCode::BAD_REQUEST);

    let edited = ts
        .client
        .put(
            "/api/profiles/launch-test/env/TOKEN",
            json!({ "value": "changed-after-preview" }),
        )
        .await
        .unwrap();
    assert_eq!(
        edited["revision"].as_i64().unwrap(),
        preview["profile_revision"].as_i64().unwrap() + 1
    );

    let response = reqwest::Client::new()
        .post(format!("http://{}/api/sessions", ts.addr))
        .json(&json!({
            "cwd": ts.cwd(),
            "goal": "must not launch drifted env",
            "selection": { "profile": "launch-test", "overrides": {} },
            "expected_profile_revision": preview["profile_revision"],
            "expected_resolver_revision": preview["resolver_revision"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["preview"]["profile_revision"], edited["revision"],
        "the conflict carries the fresh launch snapshot"
    );
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profile_capacity_admission_is_serialized_across_repositories() {
    let ts = TestServer::start().await;
    let mut profile = interactive_shell_profile("one-at-a-time");
    profile["max_concurrent"] = json!(1);
    ts.client.post("/api/profiles", profile).await.unwrap();

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
    let url = format!("http://{}/api/sessions", ts.addr);
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
        .delete(&format!(
            "/api/sessions/{}",
            session["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profile_lifetime_permit_extends_through_session_insertion() {
    let ts = TestServer::start().await;
    ts.client
        .post(
            "/api/profiles",
            interactive_shell_profile("lifetime-serialized"),
        )
        .await
        .unwrap();
    let permit = ts
        .state
        .launch_gate
        .acquire_profile("lifetime-serialized")
        .await;
    let create_url = format!("http://{}/api/sessions", ts.addr);
    let cwd = ts.cwd();
    let creating = tokio::spawn(async move {
        reqwest::Client::new()
            .post(create_url)
            .json(&json!({
                "cwd": cwd,
                "goal": "serialize profile lifetime",
                "profile": "lifetime-serialized"
            }))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let delete_url = format!("http://{}/api/profiles/lifetime-serialized", ts.addr);
    let deleting = tokio::spawn(async move {
        reqwest::Client::new()
            .delete(delete_url)
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!creating.is_finished());
    assert!(!deleting.is_finished());
    drop(permit);

    let created = tokio::time::timeout(std::time::Duration::from_secs(15), creating)
        .await
        .unwrap()
        .unwrap();
    assert!(created.status().is_success());
    let created: serde_json::Value = created.json().await.unwrap();
    let deleted = tokio::time::timeout(std::time::Duration::from_secs(15), deleting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::BAD_REQUEST);
    assert!(
        loom::profile::get(&ts.state.db, "lifetime-serialized")
            .await
            .unwrap()
            .is_some(),
        "the launch inserted its session before deletion checked references"
    );
    ts.client
        .delete(&format!(
            "/api/sessions/{}",
            created["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_clone_copies_environment_atomically_and_honors_source_revision() {
    let ts = TestServer::start().await;
    ts.client
        .post("/api/profiles", interactive_shell_profile("clone-source"))
        .await
        .unwrap();
    ts.client
        .put(
            "/api/profiles/clone-source/env/TOKEN",
            json!({ "value": "write-only-source" }),
        )
        .await
        .unwrap();
    ts.client
        .put(
            "/api/profiles/clone-source/env/REMOVE_ME",
            json!({ "value": "discarded" }),
        )
        .await
        .unwrap();
    let source = ts
        .client
        .put(
            "/api/profiles/clone-source/env/SECRET",
            json!({
                "secret_ref": "projects/acme/secrets/source/versions/latest"
            }),
        )
        .await
        .unwrap();
    let source_revision = source["revision"].as_i64().unwrap();
    let preview = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({
                "selection": {
                    "profile": "clone-source",
                    "overrides": { "effort": "" }
                }
            }),
        )
        .await
        .unwrap();

    let clone = ts
        .client
        .post(
            "/api/profiles/clone-source/clone",
            json!({
                "name": "clone-target",
                "expected_profile_revision": source_revision,
                "expected_resolver_revision": preview["resolver_revision"],
                "overrides": { "effort": "" },
                "copy_environment": false,
                "environment": {
                    "inherit": true,
                    "remove": ["REMOVE_ME"],
                    "set": [
                        { "name": "TOKEN", "value": "replaced" },
                        { "name": "ADDED", "value": "new" },
                        {
                            "name": "NEW_SECRET",
                            "secret_ref": "projects/acme/secrets/new/versions/7"
                        }
                    ]
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(clone["name"], "clone-target");
    assert_eq!(
        clone["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["ADDED", "NEW_SECRET", "SECRET", "TOKEN"]
    );
    assert_eq!(
        loom::profile::env_get(&ts.state.db, "clone-target", "TOKEN")
            .await
            .unwrap(),
        Some("replaced".to_string())
    );
    assert_eq!(
        loom::profile::env_get(&ts.state.db, "clone-target", "ADDED")
            .await
            .unwrap(),
        Some("new".to_string())
    );
    assert!(
        loom::profile::env_get(&ts.state.db, "clone-target", "REMOVE_ME")
            .await
            .unwrap()
            .is_none()
    );

    ts.client
        .put(
            "/api/profiles/clone-source/env/TOKEN",
            json!({ "value": "newer" }),
        )
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/api/profiles/clone-source/clone",
            ts.addr
        ))
        .json(&json!({
            "name": "stale-clone",
            "expected_profile_revision": source_revision,
            "expected_resolver_revision": preview["resolver_revision"],
            "overrides": {},
            "copy_environment": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["preview"]["profile_revision"],
        source_revision + 1,
        "profile drift returns a freshly resolved clone proposal"
    );
    assert!(loom::profile::get(&ts.state.db, "stale-clone")
        .await
        .unwrap()
        .is_none());
    assert!(loom::profile::env_meta(&ts.state.db, "stale-clone")
        .await
        .unwrap()
        .is_empty());

    let current = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({ "selection": { "profile": "clone-source", "overrides": {} } }),
        )
        .await
        .unwrap();
    let invalid = reqwest::Client::new()
        .post(format!(
            "http://{}/api/profiles/clone-source/clone",
            ts.addr
        ))
        .json(&json!({
            "name": "environment-rollback",
            "expected_profile_revision": current["profile_revision"],
            "expected_resolver_revision": current["resolver_revision"],
            "overrides": {},
            "environment": {
                "inherit": true,
                "remove": [],
                "set": [{
                    "name": "BROKEN",
                    "secret_ref": "not-a-secret-reference"
                }]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(loom::profile::get(&ts.state.db, "environment-rollback")
        .await
        .unwrap()
        .is_none());

    let cli = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "profile",
            "clone",
            "clone-source",
            "cli-environment-target",
            "--copy-environment",
            "--remove-environment",
            "REMOVE_ME",
            "--set-environment",
            "TOKEN=from-cli",
            "--secret-environment",
            "CLI_SECRET=projects/acme/secrets/cli/versions/latest",
        ])
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "CLI clone failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        loom::profile::env_get(&ts.state.db, "cli-environment-target", "TOKEN")
            .await
            .unwrap(),
        Some("from-cli".to_string())
    );
    let cli_meta = loom::profile::env_meta(&ts.state.db, "cli-environment-target")
        .await
        .unwrap();
    assert!(!cli_meta.iter().any(|entry| entry.name == "REMOVE_ME"));
    assert!(cli_meta.iter().any(|entry| entry.name == "CLI_SECRET"));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_clone_rejects_resolver_drift_and_accepts_an_editable_template() {
    let ts = TestServer::start().await;
    ts.client
        .post(
            "/api/profiles",
            interactive_shell_profile("editable-source"),
        )
        .await
        .unwrap();
    let preview = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({ "selection": { "profile": "editable-source", "overrides": {} } }),
        )
        .await
        .unwrap();
    loom::custom_agents::set(
        &ts.state.db,
        &loom::custom_agents::CustomAgent {
            name: "resolver-drift".to_string(),
            label: "Resolver drift".to_string(),
            setup: String::new(),
            launch: String::new(),
            resume: String::new(),
            reports_status: false,
            protocol: "terminal".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        },
    )
    .await
    .unwrap();
    let stale = reqwest::Client::new()
        .post(format!(
            "http://{}/api/profiles/editable-source/clone",
            ts.addr
        ))
        .json(&json!({
            "name": "resolver-stale-target",
            "expected_profile_revision": preview["profile_revision"],
            "expected_resolver_revision": preview["resolver_revision"],
            "overrides": {},
            "copy_environment": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale_body: serde_json::Value = stale.json().await.unwrap();
    assert!(stale_body["preview"]["resolver_revision"].is_string());
    assert_ne!(
        stale_body["preview"]["resolver_revision"],
        preview["resolver_revision"]
    );
    assert!(loom::profile::get(&ts.state.db, "resolver-stale-target")
        .await
        .unwrap()
        .is_none());

    let fresh = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({ "selection": { "profile": "editable-source", "overrides": {} } }),
        )
        .await
        .unwrap();
    let mut template = interactive_shell_profile("ignored-by-path");
    template["description"] = json!("edited before atomic clone");
    template["max_concurrent"] = json!(7);
    template["env_clear"] = json!(true);
    let created = ts
        .client
        .post(
            "/api/profiles/editable-source/clone",
            json!({
                "name": "editable-target",
                "expected_profile_revision": fresh["profile_revision"],
                "expected_resolver_revision": fresh["resolver_revision"],
                "overrides": {},
                "template": template,
                "copy_environment": false
            }),
        )
        .await
        .unwrap();
    assert_eq!(created["description"], "edited before atomic clone");
    assert_eq!(created["max_concurrent"], 7);
    assert_eq!(created["env_clear"], true);

    let race_preview = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({ "selection": { "profile": "editable-source", "overrides": {} } }),
        )
        .await
        .unwrap();
    let resolver_permit = ts.state.launch_gate.acquire_resolver().await;
    let clone_url = format!("http://{}/api/profiles/editable-source/clone", ts.addr);
    let clone_task = tokio::spawn(async move {
        reqwest::Client::new()
            .post(clone_url)
            .json(&json!({
                "name": "registry-race-target",
                "expected_profile_revision": race_preview["profile_revision"],
                "expected_resolver_revision": race_preview["resolver_revision"],
                "overrides": {},
                "copy_environment": false
            }))
            .send()
            .await
            .unwrap()
    });
    let agent_url = format!("http://{}/api/agents/custom", ts.addr);
    let mutation_task = tokio::spawn(async move {
        reqwest::Client::new()
            .post(agent_url)
            .json(&json!({
                "name": "clone-registry-race",
                "label": "Clone registry race",
                "protocol": "terminal",
                "launch": "exit 0"
            }))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!clone_task.is_finished());
    assert!(!mutation_task.is_finished());
    drop(resolver_permit);
    let clone_response = tokio::time::timeout(std::time::Duration::from_secs(10), clone_task)
        .await
        .unwrap()
        .unwrap();
    let mutation_response = tokio::time::timeout(std::time::Duration::from_secs(10), mutation_task)
        .await
        .unwrap()
        .unwrap();
    assert!(mutation_response.status().is_success());
    if clone_response.status().is_success() {
        assert!(loom::profile::get(&ts.state.db, "registry-race-target")
            .await
            .unwrap()
            .is_some());
    } else {
        assert_eq!(clone_response.status(), StatusCode::CONFLICT);
        assert!(loom::profile::get(&ts.state.db, "registry-race-target")
            .await
            .unwrap()
            .is_none());
    }
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_at_capacity_remains_cloneable() {
    let ts = TestServer::start().await;
    let mut profile = interactive_shell_profile("full-template");
    profile["max_concurrent"] = json!(1);
    ts.client.post("/api/profiles", profile).await.unwrap();
    let session = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "cwd": ts.cwd(),
                "goal": "occupy source capacity",
                "profile": "full-template"
            }),
        )
        .await
        .unwrap();
    let preview = ts
        .client
        .post(
            "/api/session-launches/resolve",
            json!({ "selection": { "profile": "full-template", "overrides": {} } }),
        )
        .await
        .unwrap();
    assert_eq!(preview["capacity"]["allowed"], false);
    assert_eq!(preview["valid"], false);

    let cloned = ts
        .client
        .post(
            "/api/profiles/full-template/clone",
            json!({
                "name": "full-template-copy",
                "expected_profile_revision": preview["profile_revision"],
                "expected_resolver_revision": preview["resolver_revision"],
                "overrides": {},
                "copy_environment": false
            }),
        )
        .await
        .unwrap();
    assert_eq!(cloned["name"], "full-template-copy");
    ts.client
        .delete(&format!(
            "/api/sessions/{}",
            session["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
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
    assert!(profile["runtime_permissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| rule == "Read(./**)"));
    assert_eq!(profile["mcp_access"]["mode"], "groups");
    assert_eq!(profile["mcp_access"]["groups"], json!(["github"]));
    assert!(profile["env"].as_array().unwrap().is_empty());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_registry_is_inspectable_without_source_access() {
    let ts = TestServer::start().await;
    let registry = ts.client.get("/api/mcps").await.unwrap();
    let set = registry["capability_sets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|set| set["name"] == "mcp/github/comment@v1")
        .expect("GitHub comment MCP set");
    assert_eq!(set["version"], "v1");
    assert!(set["digest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(set["adapter"], "github");
    assert_eq!(set["tools"].as_array().unwrap().len(), 6);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_mcp_crud_validates_source_and_profiles_select_its_group() {
    let ts = TestServer::start().await;
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
            "/api/mcps/custom",
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

    let registry = ts.client.get("/api/mcps").await.unwrap();
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
        .post("/api/profiles", profile_req.clone())
        .await
        .unwrap();
    assert_eq!(created_profile["revision"], 1);
    let effective = ts
        .client
        .get("/api/profiles/custom-tools/effective")
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
    let probe = ts
        .client
        .post("/api/profiles/custom-tools/probe", json!({}))
        .await
        .unwrap();
    assert_eq!(probe["ok"], true);

    let source_v2 = source.replace("Return a value.", "Return a pinned value.");
    let edited = ts
        .client
        .put(
            "/api/mcps/custom/ops/status",
            json!({
                "identity": "/ignored/by/put/path",
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
        .get("/api/profiles/custom-tools/effective")
        .await
        .unwrap();
    assert_eq!(
        still_pinned["mcp_policy"]["custom_servers"][0]["revision"],
        1
    );

    let reconciled = ts
        .client
        .put("/api/profiles/custom-tools", profile_req)
        .await
        .unwrap();
    assert_eq!(reconciled["revision"], 2);
    let effective = ts
        .client
        .get("/api/profiles/custom-tools/effective")
        .await
        .unwrap();
    assert_eq!(effective["mcp_policy"]["custom_servers"][0]["revision"], 2);

    let disabled = ts
        .client
        .put(
            "/api/mcps/custom/ops/status",
            json!({
                "identity": "/ignored/by/put/path",
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
    let probe = ts
        .client
        .post("/api/profiles/custom-tools/probe", json!({}))
        .await
        .unwrap();
    assert_eq!(probe["ok"], false);
    assert!(probe["errors"][0].as_str().unwrap().contains("is disabled"));

    let response = reqwest::Client::new()
        .delete(format!("http://{}/api/mcps/custom/ops/status", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("pinned by profile"));

    let response = reqwest::Client::new()
        .delete(format!("http://{}/api/profiles/custom-tools", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = reqwest::Client::new()
        .delete(format!("http://{}/api/mcps/custom/ops/status", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
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
    assert_eq!(
        session["mcp_policy"]["capability_sets"][0]["name"],
        "mcp/github/comment@v1"
    );
    assert!(session["mcp_policy"]["capability_sets"][0]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
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
    let mcp_policy: String =
        sqlx::query_scalar("SELECT policy_mcp_access FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert!(mcp_policy.contains("mcp/github/comment@v1"));
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
    assert_eq!(session["profile_revision"], 2);
    assert_eq!(session["class"], "automation");

    let delete = reqwest::Client::new()
        .delete(format!("http://{}/api/profiles/actions", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::BAD_REQUEST);

    let id = session["id"].as_str().unwrap();
    ts.client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();

    let delete = reqwest::Client::new()
        .delete(format!("http://{}/api/profiles/actions", ts.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    assert!(ts.client.get("/api/profiles/actions").await.is_err());
    assert_eq!(
        loom::profile::env_pairs(&ts.state.db, "actions")
            .await
            .unwrap(),
        vec![(
            "SECRET_TOKEN".to_string(),
            "must-not-round-trip".to_string()
        )]
    );

    let archived = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(archived["profile"], "actions");
    assert_eq!(archived["profile_revision"], 2);

    let recovered = ts
        .client
        .post(&format!("/api/sessions/{id}/recover"), json!({}))
        .await
        .unwrap();
    assert_eq!(recovered["status"], "running");
    assert_eq!(recovered["profile"], "actions");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automation_channel_reuses_one_acp_session_without_replaying_deliveries() {
    let _adapter = EnvVarGuard::set(
        "WEAVER_CLAUDE_ACP_CMD",
        &crate::fixtures::fake_acp_agent_cmd(),
    );
    let _github_token = EnvVarGuard::set("GH_TOKEN", "test-token");
    let ts = TestServer::start().await;
    ts.client
        .post(
            "/api/profiles",
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
        .post("/api/runs", first_request.clone())
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
        .post("/api/runs", second_request.clone())
        .await
        .unwrap();
    let duplicate = ts.client.post("/api/runs", second_request).await.unwrap();
    let mut collision_request = first_request;
    collision_request["channel"] = json!("another-operator");
    collision_request["session"]["goal"] = json!("must not be delivered");
    let collision = ts
        .client
        .post("/api/runs", collision_request)
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
        .get("/api/sessions?automation=true")
        .await
        .unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);

    let chat = ts
        .client
        .get(&format!(
            "/api/sessions/{}/chat",
            first["session_id"].as_str().unwrap()
        ))
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deployment_manifest_reconciles_profiles_secret_refs_and_workload_identity() {
    let ts = TestServer::start().await;
    let manifest = json!({
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
    assert_eq!(first["profiles"][0]["revision"], 2);
    assert_eq!(
        first["profiles"][0]["mcp_access"],
        json!({"mode": "groups", "groups": ["messaging"]})
    );
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
    assert_eq!(second["profiles"][0]["revision"], 2);
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

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deployment_reconcile_serializes_with_registry_mutation() {
    let ts = TestServer::start().await;
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
    let mutation_url = format!("http://{}/api/agents/custom/shell", ts.addr);
    let mutation = tokio::spawn(async move {
        reqwest::Client::new()
            .put(mutation_url)
            .json(&json!({
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
