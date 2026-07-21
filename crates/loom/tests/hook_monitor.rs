//! Writing a `hook` event row drives the monitor to flip session status on
//! its next tick. This mirrors what `weaver hook --event …` does in practice
//! (writes an event row, no HTTP); see the `weaver` crate's `agent_cli`
//! integration test for the binary-driven side.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use loom::client;
use loom::events::EventBus;
use loom::session as session_mod;
use loom::web::AppState;
use loom::{db, server};
use serde_json::json;
use tokio::net::TcpListener;

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

/// The `tapestry` supervisor binary built beside this test binary (two levels up
/// from `target/<profile>/deps/<bin>`). loom's `backend` reads
/// `WEAVER_TAPESTRY_BIN` to launch it.
fn tapestry_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    exe.parent()
        .and_then(Path::parent)
        .expect("target dir")
        .join("tapestry")
}

/// Best-effort: kill every supervisor whose socket lives under `sock_dir` with a
/// raw `KILL` frame (`u32`-BE length `1` + the `KILL` opcode `0x13`), so detached
/// terminal processes don't outlive the test. Synchronous, so it is safe in
/// `Drop` inside the tokio runtime.
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

/// Temp `WEAVER_HOME` that kills its terminal supervisors on drop.
struct TestHome(tempfile::TempDir);

impl Drop for TestHome {
    fn drop(&mut self) {
        kill_supervisors_in(&self.0.path().join("sock"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_event_drives_session_status() {
    let home = TestHome(tempfile::tempdir().unwrap());
    std::env::set_var("WEAVER_HOME", home.0.path());
    std::env::set_var("WEAVER_TAPESTRY_BIN", tapestry_bin());
    // `seed_owner` no longer defaults to a real login — this test's requests
    // ride loopback trust, which needs a seeded owner to resolve to.
    std::env::set_var("LOOM_OWNER_GITHUB", "rjpower");

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
    // The session below launches with `"agent": "shell"`; `shell` is no longer a
    // builtin, so seed it as a command-less custom agent (execs a bare login
    // shell, hookless).
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
    };
    tokio::spawn(server::serve(state, listener));

    std::env::set_var("WEAVER_API", format!("http://{addr}"));
    let client = client::default();
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
    weaver_core::events::record_local(&pool, &branch_id, "hook", json!({ "event": "working" }))
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
    assert_eq!(
        got, "running",
        "monitor should have flipped status to running"
    );
    // The same `working` edge counts one agent turn. The hookless shell agent
    // is `running` from create, so the status poll above can return before the
    // monitor tick that drains the hook row — poll the count separately.
    let mut turns = 0;
    for _ in 0..40 {
        turns = session_mod::get(&pool, &id)
            .await
            .unwrap()
            .unwrap()
            .turn_count;
        if turns == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(turns, 1, "a working hook counts one turn");

    // A `waiting` hook (a quiet lull) stamps the soothing, *quiet* `idle` mark —
    // the calm "resting, no one needed" state, never the loud `attention` axis,
    // so an idle agent doesn't read as needing the user.
    weaver_core::events::record_local(&pool, &branch_id, "hook", json!({ "event": "waiting" }))
        .await
        .unwrap();
    let mut idle = String::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ws = client.get(&format!("/api/sessions/{id}")).await.unwrap();
        let tags = ws["branch"]["tags"].as_array().cloned().unwrap_or_default();
        idle = tags
            .iter()
            .find(|t| t["key"] == "idle")
            .and_then(|t| t["value"].as_str())
            .unwrap_or("")
            .to_string();
        // It must NOT raise the loud attention axis.
        assert!(
            !tags.iter().any(|t| t["key"] == "attention"),
            "waiting hook must not raise the loud attention tag"
        );
        if idle == "idle" {
            break;
        }
    }
    assert_eq!(
        idle, "idle",
        "waiting hook should stamp the soothing idle mark"
    );

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
    assert!(
        kinds.contains(&"hook"),
        "events should include a hook row: {kinds:?}"
    );
}

/// Seed a branch + session of the given `class` / `managed_by` and return the
/// branch id. Class rides a direct UPDATE: launch paths own the column's
/// provenance, and this test only needs the stored value.
async fn seed_classed_session(
    db: &loom::db::Db,
    id: &str,
    branch_name: &str,
    class: &str,
    managed_by: Option<&str>,
) -> String {
    let branch = weaver_core::branch::upsert(db, "/r", branch_name, "main")
        .await
        .unwrap();
    session_mod::insert(
        db,
        &session_mod::NewSession {
            id: id.to_string(),
            branch_id: branch.id.clone(),
            work_dir: "/w".to_string(),
            term_session: format!("weaver-{id}"),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: managed_by.map(str::to_string),
            created_by: None,
            protocol: "acp".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE sessions SET class = ? WHERE id = ?")
        .bind(class)
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    branch.id
}

/// Exceeding `automation.turn_cap` on an automation-class session marks the
/// branch `blocked`; warm (watch-managed) sessions are exempt. Drives the same
/// `promote_lifecycle` path the hook consumer uses, via the ACP entry point —
/// no server or monitor sleeps needed.
#[tokio::test]
async fn turn_cap_blocks_automation_session() {
    let pool = db::connect_in_memory().await.unwrap();
    let bus = EventBus::new();
    weaver_core::config::apply(&pool, &[("automation.turn_cap".into(), Some("1".into()))])
        .await
        .unwrap();

    let branch_id = seed_classed_session(&pool, "cap1", "weaver/cap", "automation", None).await;

    // Turn 1 is within the cap: no attention tag raised.
    loom::monitor::record_acp_lifecycle(&pool, &bus, "cap1", "working").await;
    let s = session_mod::get(&pool, "cap1").await.unwrap().unwrap();
    assert_eq!(s.turn_count, 1, "working edge counts a turn");
    assert!(
        weaver_core::tags::get(&pool, &branch_id, weaver_core::tags::ATTENTION_KEY)
            .await
            .unwrap()
            .is_none(),
        "within the cap the attention axis stays calm"
    );

    // Turn 2 exceeds the cap: blocked, with the cap note.
    loom::monitor::record_acp_lifecycle(&pool, &bus, "cap1", "working").await;
    let tag = weaver_core::tags::get(&pool, &branch_id, weaver_core::tags::ATTENTION_KEY)
        .await
        .unwrap()
        .expect("over the cap the branch is marked blocked");
    assert_eq!(tag.value, "blocked");
    assert!(
        tag.note.contains("turn cap (1) reached"),
        "note carries the cap: {}",
        tag.note
    );

    // A warm (watch-managed) automation session is exempt infrastructure: the
    // count still advances but the cap never marks it blocked.
    let warm_branch =
        seed_classed_session(&pool, "warm1", "weaver/warm", "automation", Some("w1")).await;
    loom::monitor::record_acp_lifecycle(&pool, &bus, "warm1", "working").await;
    loom::monitor::record_acp_lifecycle(&pool, &bus, "warm1", "working").await;
    let warm = session_mod::get(&pool, "warm1").await.unwrap().unwrap();
    assert_eq!(warm.turn_count, 2, "warm sessions still count turns");
    assert!(
        weaver_core::tags::get(&pool, &warm_branch, weaver_core::tags::ATTENTION_KEY)
            .await
            .unwrap()
            .is_none(),
        "warm sessions are never capped"
    );
}
