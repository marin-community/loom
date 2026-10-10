//! Repository files cannot change the launch environment or execute setup.

use serde_json::json;
use serial_test::serial;
use std::path::Path;
use std::time::Duration;

use crate::fixtures::{sh, TestServer};

async fn observed_environment(work_dir: &Path) -> String {
    let path = work_dir.join("observed-env");
    for _ in 0..100 {
        if path.exists() {
            return std::fs::read_to_string(path).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("agent did not record its launch environment");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repository_config_is_ignored_on_launch_and_recovery() {
    let ts = TestServer::start_with_app().await;
    let config_dir = ts.repo_path().join(".weaver");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[setup]
script = "touch setup-marker; exit 7"
[env]
TOOL_SETTING = "repository-file"
CODEX_CONFIG = "repository-policy"
[agent]
default = "unknown-agent"
model = "unknown-model"
effort = "unknown-effort"
"#,
    )
    .unwrap();
    sh(ts.repo_path(), "git", &["add", ".weaver/config.toml"]);
    sh(
        ts.repo_path(),
        "git",
        &["commit", "-m", "add obsolete repository config"],
    );
    loom::repo::register(
        &ts.state.db,
        "acme/widgets",
        "https://example/acme/widgets.git",
        &ts.cwd(),
    )
    .await
    .unwrap();
    let mut profile = loom::profile::get(&ts.state.db, "default")
        .await
        .unwrap()
        .unwrap()
        .as_input()
        .unwrap();
    profile.agent_kind = "shell".into();
    profile.protocol = "terminal".into();
    profile.github_repositories = vec!["acme/widgets".into()];
    loom::profile::upsert(&ts.state.db, &profile).await.unwrap();
    loom::profile::env_set(&ts.state.db, "default", "TOOL_SETTING", "profile-tool")
        .await
        .unwrap();
    loom::profile::env_set(&ts.state.db, "default", "CODEX_CONFIG", "operator-policy")
        .await
        .unwrap();
    loom::repo_env::set(&ts.state.db, &ts.cwd(), "TOOL_SETTING", "repository-tool")
        .await
        .unwrap();
    // A stored value from before validation was added must not reach the agent.
    loom::repo_env::set(
        &ts.state.db,
        &ts.cwd(),
        "CODEX_CONFIG",
        "stored-repository-policy",
    )
    .await
    .unwrap();
    let mut agent = loom::custom_agents::get(&ts.state.db, "shell")
        .await
        .unwrap()
        .unwrap();
    agent.launch =
        r#"printf '%s\n' "$TOOL_SETTING" "$CODEX_CONFIG" > observed-env.tmp; mv observed-env.tmp observed-env; exec sh -s --"#.into();
    loom::custom_agents::set(&ts.state.db, &agent)
        .await
        .unwrap();

    let created = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({"goal":"ignore repository config", "cwd":ts.cwd()}),
        )
        .await
        .unwrap();
    assert_eq!(created["agent_kind"], "shell");
    assert_eq!(created["status"], "running");
    let work_dir = Path::new(created["work_dir"].as_str().unwrap());
    assert_eq!(
        observed_environment(work_dir).await,
        "repository-tool\noperator-policy\n"
    );
    assert!(!work_dir.join("setup-marker").exists());

    std::fs::remove_file(work_dir.join("observed-env")).unwrap();
    let id = created["id"].as_str().unwrap();
    ts.client
        .post("/api/sessions/archive", json!({"session":id}))
        .await
        .unwrap();
    std::fs::write(config_dir.join("config.toml"), "= invalid TOML").unwrap();
    ts.client
        .post("/api/sessions/recover", json!({"session":id}))
        .await
        .unwrap();
    assert_eq!(
        observed_environment(work_dir).await,
        "repository-tool\noperator-policy\n"
    );
    assert!(!work_dir.join("setup-marker").exists());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repository_environment_api_rejects_codex_controls() {
    let ts = TestServer::start().await;
    for name in [
        "CODEX_CONFIG",
        "CODEX_HOME",
        "INITIAL_AGENT_MODE",
        "DEFAULT_AUTH_REQUEST",
    ] {
        let result = ts
            .client
            .post(
                "/api/repos/env/set",
                json!({"cwd":ts.cwd(), "name":name, "value":"repository-value"}),
            )
            .await;
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("reserved for administrator-managed"),
            "{name}: {error}"
        );
    }
    ts.client
        .post(
            "/api/repos/env/set",
            json!({"cwd":ts.cwd(), "name":"UV_CACHE_DIR", "value":"/shared/uv"}),
        )
        .await
        .unwrap();
    let values = ts
        .client
        .post("/api/repos/env/get", json!({"cwd":ts.cwd()}))
        .await
        .unwrap();
    let names: Vec<_> = values["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["UV_CACHE_DIR"]);
}
