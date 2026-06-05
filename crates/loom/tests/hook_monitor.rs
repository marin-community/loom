//! Writing a `hook` event row drives the monitor to flip session status on
//! its next tick. This mirrors what `weaver hook --event …` does in practice
//! (writes an event row, no HTTP); see the `weaver` crate's `agent_cli`
//! integration test for the binary-driven side.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use tokio::net::TcpListener;
use loom::client::Client;
use loom::events::EventBus;
use loom::session as session_mod;
use loom::web::AppState;
use loom::{db, server};

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

/// Pins tmux to a throwaway server (`tmux -L <name>`) for the test and kills it
/// on drop, so the suite never touches the user's real sessions. See
/// `loom::tmux::socket_args`.
struct TmuxSocket(String);

impl TmuxSocket {
    fn install() -> Self {
        let name = format!("weaver-test-{}", std::process::id());
        std::env::set_var("WEAVER_TMUX_SOCKET", &name);
        let _ = Command::new("tmux").args(["-L", &name, "kill-server"]).status();
        TmuxSocket(name)
    }
}

impl Drop for TmuxSocket {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-L", &self.0, "kill-server"]).status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_event_drives_session_status() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("WEAVER_HOME", home.path());
    let _tmux = TmuxSocket::install();

    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git", &["init", "-b", "main"]);
    sh(repo.path(), "git", &["config", "user.email", "t@t.test"]);
    sh(repo.path(), "git", &["config", "user.name", "Test"]);
    std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
    sh(repo.path(), "git", &["add", "."]);
    sh(repo.path(), "git", &["commit", "-m", "init"]);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let pool = db::connect(&db::default_db_path()).await.unwrap();
    let state = AppState {
        db: pool.clone(),
        bus: EventBus::new(),
        addr: addr.to_string(),
    };
    tokio::spawn(server::serve(state, listener));

    std::env::set_var("WEAVER_API", format!("http://{addr}"));
    let client = Client::new();
    for _ in 0..60 {
        if client.get("/api/health").await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Spawn a session (shell agent — no real claude needed).
    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "hook smoke test",
                "cwd": repo.path().to_string_lossy(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let branch_id = {
        let session = session_mod::get(&pool, &id).await.unwrap().unwrap();
        session.branch_id
    };

    // What `weaver hook --event working` would do: write a `hook` event row.
    // Any hook means the agent process is alive → lifecycle `running`.
    weaver_core::events::record_local(
        &pool,
        &branch_id,
        "hook",
        json!({ "event": "working" }),
    )
    .await
    .unwrap();

    // Wait up to a couple of monitor ticks for the status to flip.
    let mut got = String::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ws = client.get(&format!("/api/sessions/{id}")).await.unwrap();
        got = ws["status"].as_str().unwrap_or("").to_string();
        if got == "running" {
            break;
        }
    }
    assert_eq!(got, "running", "monitor should have flipped status to running");

    // A `waiting` hook (Claude blocked asking the user) raises the agent-declared
    // attention axis to `attention` with a note — the dashboard's "needs me" flag.
    weaver_core::events::record_local(&pool, &branch_id, "hook", json!({ "event": "waiting" }))
        .await
        .unwrap();
    let mut attention = String::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ws = client.get(&format!("/api/sessions/{id}")).await.unwrap();
        attention = ws["branch"]["attention"].as_str().unwrap_or("").to_string();
        if attention == "attention" {
            assert_eq!(ws["branch"]["attention_note"], "Waiting for input");
            break;
        }
    }
    assert_eq!(attention, "attention", "waiting hook should raise attention");

    // Verify the hook event row landed too.
    let log = client
        .get(&format!("/api/sessions/{id}/log"))
        .await
        .unwrap();
    let kinds: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"hook"), "events should include a hook row: {kinds:?}");
}
