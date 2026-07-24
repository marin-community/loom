//! Per-repo `.weaver/config.toml` `[setup]` execution and `[env]` layering at
//! session launch (shared-loom design §6.4).
//!
//! These drive the real create-session path through an in-process loom server: a
//! committed `.weaver/config.toml` is read, its setup script runs in the worktree
//! for an *allowlisted* repo (and is skipped, visibly, for one that isn't), and
//! the resolved environment (global < per-repo < repo-file) is what the script
//! sees. Mirrors the `hook_monitor` integration harness.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use loom::events::EventBus;
use loom::session as session_mod;
use loom::web::AppState;
use loom::{agent_env, client, db, repo, repo_env, server};
use serde_json::{json, Value};
use serial_test::serial;
use tokio::net::TcpListener;

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

fn tapestry_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    exe.parent()
        .and_then(Path::parent)
        .expect("target dir")
        .join("tapestry")
}

/// Best-effort: kill every supervisor under `sock_dir` so detached terminals
/// don't outlive the test (raw `KILL` frame, like the hook_monitor harness).
fn kill_supervisors_in(sock_dir: &Path) {
    use std::io::Write;
    let Ok(entries) = std::fs::read_dir(sock_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sock") {
            continue;
        }
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&path) {
            let _ = stream.write_all(&[0, 0, 0, 1, 0x13]);
            let _ = stream.flush();
        }
    }
}

struct TestHome(tempfile::TempDir);

impl Drop for TestHome {
    fn drop(&mut self) {
        kill_supervisors_in(&self.0.path().join("sock"));
    }
}

/// A git repo that ships `.weaver/config.toml` with `config_body`. Returns the
/// canonical repo path.
fn repo_with_config(config_body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    sh(path, "git", &["init", "-b", "main"]);
    sh(path, "git", &["config", "user.email", "t@t.test"]);
    sh(path, "git", &["config", "user.name", "Test"]);
    std::fs::create_dir_all(path.join(".weaver")).unwrap();
    std::fs::write(path.join(".weaver/config.toml"), config_body).unwrap();
    std::fs::write(path.join("README.md"), "hi\n").unwrap();
    sh(path, "git", &["add", "."]);
    sh(path, "git", &["commit", "-m", "init"]);
    let canonical = path.canonicalize().unwrap();
    (dir, canonical)
}

/// Bring up an in-process loom server on a fresh temp WEAVER_HOME; returns the
/// home guard, the client, and the pool.
async fn start_server() -> (TestHome, loom::client::Client, db::Db, String) {
    let home = TestHome(tempfile::tempdir().unwrap());
    std::env::set_var("WEAVER_HOME", home.0.path());
    std::env::set_var("WEAVER_TAPESTRY_BIN", tapestry_bin());
    // `seed_owner` no longer defaults to a real login — this suite's requests
    // ride loopback trust, which needs a seeded owner to resolve to.
    std::env::set_var("LOOM_OWNER_GITHUB", "rjpower");
    std::env::remove_var("LOOM_TOKEN");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let pool = db::connect(&db::default_db_path()).await.unwrap();
    // `create_shell_session` below launches with `"agent": "shell"`; `shell` is no
    // longer a builtin, so seed it as a command-less custom agent (execs a bare
    // login shell, hookless → `running` immediately).
    loom::custom_agents::set(
        &pool,
        &loom::custom_agents::CustomAgent {
            name: "shell".to_string(),
            label: "Shell".to_string(),
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
    let state = AppState {
        db: pool.clone(),
        bus: EventBus::new(),
        addr: addr.to_string(),
        ide: std::sync::Arc::new(loom::ide::IdeManager::new(loom::ide::ide_home())),
        trigger: loom::github_trigger::GithubTrigger::production(pool.clone()),
        acp: loom::acp::AcpRegistry::new(),
        launch_gate: loom::launch_gate::RepoLaunchGate::default(),
    };
    tokio::spawn(server::serve(state, listener));

    std::env::set_var("WEAVER_API", format!("http://{addr}"));
    let client = client::Client::new(format!("http://{addr}"));
    for _ in 0..60 {
        if client.get("/api/health").await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (home, client, pool, addr.to_string())
}

async fn create_shell_session(client: &client::Client, cwd: &Path) -> Value {
    client
        .post(
            "/api/sessions",
            json!({ "goal": "setup test", "cwd": cwd.to_string_lossy(), "agent": "shell" }),
        )
        .await
        .unwrap()
}

/// Every `setup` lifecycle event recorded for a session, newest-or-oldest order
/// as returned by the log endpoint.
async fn setup_events(client: &client::Client, id: &str) -> Vec<Value> {
    let log = client
        .get(&format!("/api/sessions/{id}/log"))
        .await
        .unwrap();
    log.as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "setup")
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn setup_runs_for_allowlisted_repo() {
    let (_home, client, pool, _addr) = start_server().await;
    let (_dir, repo_root) =
        repo_with_config("[setup]\nscript = \"echo SETUP_RAN\\ntouch setup-marker\"\n");

    // Allowlist the repo (register its path as a managed repo).
    repo::register(
        &pool,
        "acme/widgets",
        "https://example/acme/widgets.git",
        &repo_root.to_string_lossy(),
    )
    .await
    .unwrap();

    let ws = create_shell_session(&client, &repo_root).await;
    let id = ws["id"].as_str().unwrap().to_string();
    let session = session_mod::get(&pool, &id).await.unwrap().unwrap();
    let work_dir = PathBuf::from(&session.work_dir);

    // The setup script ran in the worktree: its marker is there.
    assert!(
        work_dir.join("setup-marker").exists(),
        "setup should have created its marker in the worktree {}",
        work_dir.display()
    );
    // The session launched normally (setup succeeded).
    assert_eq!(session.status, "running");

    // Full output was captured to the run dir's setup.log…
    let log_path = db::run_dir(&id).join("setup.log");
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(log.contains("SETUP_RAN"), "setup.log: {log}");

    // …and the lifecycle is observable as `setup` events on the session.
    let events = setup_events(&client, &id).await;
    assert!(
        events.iter().any(|e| e["data"]["phase"] == "started"),
        "expected a setup 'started' event: {events:?}"
    );
    let finished = events
        .iter()
        .find(|e| e["data"]["phase"] == "finished")
        .expect("a setup 'finished' event");
    assert_eq!(finished["data"]["success"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn setup_failure_marks_session_error() {
    let (_home, client, pool, _addr) = start_server().await;
    let (_dir, repo_root) = repo_with_config("[setup]\nscript = \"echo BOOM\\nexit 3\"\n");
    repo::register(
        &pool,
        "acme/widgets",
        "https://example/acme/widgets.git",
        &repo_root.to_string_lossy(),
    )
    .await
    .unwrap();

    let ws = create_shell_session(&client, &repo_root).await;
    let id = ws["id"].as_str().unwrap().to_string();

    // A failed setup leaves the session in a visible error state, not running.
    assert_eq!(ws["status"], "error", "view status should be error: {ws}");
    let session = session_mod::get(&pool, &id).await.unwrap().unwrap();
    assert_eq!(session.status, "error");

    // It raises the loud attention axis so the dashboard flags it.
    let detail = client.get(&format!("/api/sessions/{id}")).await.unwrap();
    let tags = detail["branch"]["tags"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let attention = tags.iter().find(|t| t["key"] == "attention");
    assert!(
        attention.is_some_and(|t| t["value"] == "blocked"),
        "setup failure should raise attention=blocked: {tags:?}"
    );

    // The finished event records the failure with the real exit code.
    let finished = setup_events(&client, &id)
        .await
        .into_iter()
        .find(|e| e["data"]["phase"] == "finished")
        .expect("a setup 'finished' event");
    assert_eq!(finished["data"]["success"], false);
    assert_eq!(finished["data"]["exit_code"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn non_allowlisted_repo_does_not_run_setup() {
    let (_home, client, pool, _addr) = start_server().await;
    // Same setup that would write a marker — but the repo is NOT registered.
    let (_dir, repo_root) = repo_with_config("[setup]\nscript = \"touch setup-marker\"\n");

    let ws = create_shell_session(&client, &repo_root).await;
    let id = ws["id"].as_str().unwrap().to_string();
    let session = session_mod::get(&pool, &id).await.unwrap().unwrap();
    let work_dir = PathBuf::from(&session.work_dir);

    // The setup script never ran: no marker, and the session launched normally.
    assert!(
        !work_dir.join("setup-marker").exists(),
        "setup must NOT run for a non-allowlisted repo"
    );
    assert_eq!(session.status, "running");

    // The skip is recorded (and there is no finished event).
    let events = setup_events(&client, &id).await;
    assert!(
        events.iter().any(|e| e["data"]["phase"] == "skipped"),
        "expected a setup 'skipped' event: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e["data"]["phase"] == "finished"),
        "setup should not have run: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn env_layers_global_then_repo_then_config_file() {
    let (_home, client, pool, _addr) = start_server().await;
    // The config-file [env] layer is highest priority; the setup script (the
    // `commands` list form) echoes the resolved values so we can read the
    // precedence end-to-end.
    let config = "[setup]\n\
        commands = [\n\
        \"echo FROM_GLOBAL=$FROM_GLOBAL > env-marker\",\n\
        \"echo SHARED=$SHARED >> env-marker\",\n\
        \"echo ONLY_REPO=$ONLY_REPO >> env-marker\",\n\
        \"echo ONLY_CONFIG=$ONLY_CONFIG >> env-marker\",\n\
        ]\n\n\
        [env]\nSHARED = \"from-config\"\nONLY_CONFIG = \"config-value\"\n";
    let (_dir, repo_root) = repo_with_config(config);

    // Global agent_env: a unique var and one shared name.
    agent_env::set(&pool, "FROM_GLOBAL", "global-value")
        .await
        .unwrap();
    agent_env::set(&pool, "SHARED", "from-global")
        .await
        .unwrap();
    // Per-repo env: overrides SHARED, adds its own.
    repo_env::set(&pool, &repo_root.to_string_lossy(), "SHARED", "from-repo")
        .await
        .unwrap();
    repo_env::set(
        &pool,
        &repo_root.to_string_lossy(),
        "ONLY_REPO",
        "repo-value",
    )
    .await
    .unwrap();

    repo::register(
        &pool,
        "acme/widgets",
        "https://example/acme/widgets.git",
        &repo_root.to_string_lossy(),
    )
    .await
    .unwrap();

    let ws = create_shell_session(&client, &repo_root).await;
    let id = ws["id"].as_str().unwrap().to_string();
    let session = session_mod::get(&pool, &id).await.unwrap().unwrap();
    let marker = std::fs::read_to_string(PathBuf::from(&session.work_dir).join("env-marker"))
        .expect("setup should have written env-marker");
    let marker = marker.trim();

    // Global-only passes through; SHARED resolves to the config-file layer (the
    // highest); the repo-only and config-only vars are present.
    assert!(marker.contains("FROM_GLOBAL=global-value"), "{marker}");
    assert!(marker.contains("SHARED=from-config"), "{marker}");
    assert!(marker.contains("ONLY_REPO=repo-value"), "{marker}");
    assert!(marker.contains("ONLY_CONFIG=config-value"), "{marker}");
}
