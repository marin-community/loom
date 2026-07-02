//! Shared harness for the loom integration suites: a real server bound to a
//! random port, backed by an isolated weaver home (which also isolates the
//! `tapestry` terminal sockets), and a throwaway git repo. See the sibling
//! modules (`sessions`, `terminal`, `scratch`, `files`, `branches`, `archive`)
//! for the focused cases.
//!
//! `TestServer::start` mutates process-global env (`WEAVER_HOME` /
//! `WEAVER_API` / `WEAVER_TAPESTRY_BIN`), so every test that uses it is marked
//! `#[serial]` — they share one binary and would otherwise race on that env.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use futures_util::SinkExt;
use loom::client::{self, Client};
use loom::events::EventBus;
use loom::web::AppState;
use loom::{db, server};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use weaver_core::config as core_config;

/// Run `program args` in `dir`, asserting it succeeds.
pub fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

/// The `tapestry` supervisor binary built alongside this test binary. The
/// integration test runner lives at `target/<profile>/deps/<bin>`; the sibling
/// `tapestry` binary is two levels up at `target/<profile>/tapestry`. loom's
/// `backend` reads `WEAVER_TAPESTRY_BIN` to launch it (so it does not try to
/// re-exec the test harness as a supervisor).
fn tapestry_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .expect("target dir")
        .join("tapestry");
    assert!(
        bin.exists(),
        "tapestry binary missing at {} — run via `cargo test --workspace` (or `cargo build -p tapestry` first)",
        bin.display()
    );
    bin
}

/// Best-effort teardown: kill every supervisor whose socket lives under this
/// test's home, so its detached terminal processes don't outlive the test. Sends
/// a raw `KILL` frame (`u32`-BE length `1` + the `KILL` opcode `0x13`) on each
/// socket — synchronous so it is safe to call from `Drop` inside the tokio
/// runtime (where spinning a new runtime would panic).
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

/// Seed the command-less `shell` custom agent the suites launch no-op sessions
/// with. `shell` is no longer a builtin; a custom agent with no `launch` command
/// execs a bare login shell (hookless → `running` at once), which is exactly the
/// cheap session the tests want.
pub async fn seed_shell_agent(db: &loom::db::Db) {
    loom::custom_agents::set(
        db,
        &loom::custom_agents::CustomAgent {
            name: "shell".to_string(),
            label: "Shell".to_string(),
            setup: String::new(),
            launch: String::new(),
            resume: String::new(),
            reports_status: false,
            created_at: String::new(),
            updated_at: String::new(),
        },
    )
    .await
    .unwrap();
}

/// Build a throwaway git repo with a single commit on `main`.
fn init_repo(dir: &Path) {
    sh(dir, "git", &["init", "-b", "main"]);
    sh(dir, "git", &["config", "user.email", "t@t.test"]);
    sh(dir, "git", &["config", "user.name", "Test"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    sh(dir, "git", &["add", "."]);
    sh(dir, "git", &["commit", "-m", "init"]);
}

/// A running loom server with fully isolated state: its own temp `WEAVER_HOME`
/// (and therefore its own sqlite db and `tapestry` socket dir), and a throwaway
/// git repo. Drop kills any supervisors it spawned; the temp dirs clean
/// themselves up.
pub struct TestServer {
    pub client: Client,
    pub addr: SocketAddr,
    /// The running server's embedded-editor manager (shared `Arc`). The IDE
    /// proxy tests register a stub upstream on it instead of spawning a real
    /// code-server.
    pub ide: std::sync::Arc<loom::ide::IdeManager>,
    repo: TempDir,
    _home: TempDir,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Tear down detached terminal supervisors before the home (and its
        // sockets) is removed, so they don't outlive the test.
        kill_supervisors_in(&self._home.path().join("sock"));
    }
}

impl TestServer {
    /// Boot a server on a random port with isolated state and wait for it to
    /// answer `/api/health`. The caller must be `#[serial]`: setup writes
    /// process-global env.
    pub async fn start() -> Self {
        Self::start_inner(None).await
    }

    /// Like [`start`](Self::start) but installs `gh` as the GitHub trigger's
    /// gateway, so the webhook suite can drive the permission check + reply with
    /// a fake instead of the real `gh` CLI.
    pub async fn start_with_github(
        gh: std::sync::Arc<dyn loom::github_trigger::GithubApi>,
    ) -> Self {
        Self::start_inner(Some(gh)).await
    }

    async fn start_inner(gh: Option<std::sync::Arc<dyn loom::github_trigger::GithubApi>>) -> Self {
        // Isolate weaver state in a temp home (its own db) for the lifetime of
        // the test; that home also scopes the `tapestry` socket dir. Point loom
        // at the sibling supervisor binary.
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        std::env::set_var("WEAVER_TAPESTRY_BIN", tapestry_bin());
        // `seed_owner` no longer defaults to a real login — the suite's requests
        // ride loopback trust, which needs a seeded owner to resolve to.
        std::env::set_var("LOOM_OWNER_GITHUB", "rjpower");

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pool = db::connect(&db::default_db_path()).await.unwrap();
        let trigger = match gh {
            Some(gh) => loom::github_trigger::GithubTrigger::with_gateway(gh),
            None => loom::github_trigger::GithubTrigger::production(pool.clone()),
        };
        let state = AppState {
            db: pool,
            bus: EventBus::new(),
            addr: addr.to_string(),
            ide: std::sync::Arc::new(loom::ide::IdeManager::new(loom::ide::ide_home())),
            trigger,
        };
        // The watch master switch ships on by default, but these tests
        // drive the engine directly and must not race the background loop that
        // `server::serve` spawns. Pin it off so the daemon's own engine idles.
        core_config::apply(
            &state.db,
            &[("watch.enabled".to_string(), Some("false".to_string()))],
        )
        .await
        .unwrap();
        // The suites launch cheap no-op sessions with `"agent": "shell"`. `shell`
        // is no longer a builtin, so seed it as a command-less custom agent: it
        // execs a bare login shell and is hookless (so it comes up `running`
        // immediately, never stuck `launching`).
        seed_shell_agent(&state.db).await;
        // Keep a handle to the editor manager before `state` moves into serve, so
        // a test can register a stub upstream on the same instance.
        let ide = state.ide.clone();
        tokio::spawn(server::serve(state, listener));

        std::env::set_var("WEAVER_API", format!("http://{addr}"));
        // Pin the one-shot agent to a fast no-op: `true` exits 0 with empty
        // output, so a judgement degrades to the deterministic fallback
        // rather than spawning a real (slow, environment-dependent) claude.
        // A test exercising the agent path overrides this itself.
        std::env::set_var("WEAVER_WATCH_AGENT_CMD", "true");
        let client = client::default();
        for _ in 0..60 {
            if client.get("/api/health").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Self {
            client,
            addr,
            ide,
            repo,
            _home: home,
        }
    }

    /// Path to the throwaway repo used as `cwd` when creating sessions.
    pub fn repo_path(&self) -> &Path {
        self.repo.path()
    }

    /// `repo_path()` as the lossy string the API expects in `cwd`.
    pub fn cwd(&self) -> String {
        self.repo.path().to_string_lossy().into_owned()
    }
}

/// Restores `$HOME` on drop, so a test that points it at a temp dir (to exercise
/// transcript capture / the conversation view, which read `~/.claude`) can't leak
/// that into siblings. The suite is `#[serial]`, so the swap is safe.
pub struct HomeGuard(Option<std::ffi::OsString>);
impl HomeGuard {
    pub fn set(path: &Path) -> Self {
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        HomeGuard(prev)
    }
}
impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Plant a minimal two-turn Claude transcript where the agent would have written
/// it for `work_dir` (`$HOME/.claude/projects/<munged-cwd>/session.jsonl`), so a
/// test can exercise capture / the conversation view without a real agent run.
pub fn plant_claude_transcript(home: &Path, work_dir: &str, user: &str, assistant: &str) {
    let proj = home.join(".claude").join("projects").join(
        weaver_core::transcript::claude_project_dir_name(Path::new(work_dir)),
    );
    std::fs::create_dir_all(&proj).unwrap();
    let transcript = [
        serde_json::json!({"type": "user", "sessionId": "abc",
            "timestamp": "2026-06-17T10:00:00.000Z",
            "message": {"role": "user", "content": user}}),
        serde_json::json!({"type": "assistant", "sessionId": "abc",
            "timestamp": "2026-06-17T10:00:01.000Z",
            "message": {"role": "assistant", "model": "claude-opus-4-8",
                        "content": [{"type": "text", "text": assistant}]}}),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    std::fs::write(proj.join("session.jsonl"), transcript).unwrap();
}

/// One tag off a `SessionView` (or `BranchView`) JSON `branch.tags` array by
/// key, or `None` when the branch carries no tag for that key. The status axes
/// — the agent's `attention` and a watch's `triage` — are tags, so this is
/// how a test reads a level/note/author off the wire.
pub fn branch_tag<'a>(view: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    view.get("branch")
        .and_then(|b| b.get("tags"))
        .and_then(|t| t.as_array())
        .and_then(|tags| {
            tags.iter()
                .find(|t| t.get("key").and_then(|k| k.as_str()) == Some(key))
        })
}

/// The value of a `branch.tags` tag by key, or `""` when absent — the resolved
/// level for the loud keys (absence is the calm `ok` state, so an unmarked axis
/// reads as empty).
pub fn branch_tag_value<'a>(view: &'a serde_json::Value, key: &str) -> &'a str {
    branch_tag(view, key)
        .and_then(|t| t.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// Terminal WebSocket helpers (used by `terminal.rs`)
// ---------------------------------------------------------------------------

pub type TermWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect a terminal WebSocket to a session. No `Origin` header is sent, so the
/// server's same-origin check takes the missing-Origin (non-browser) path.
pub async fn connect_terminal(addr: &SocketAddr, id: &str) -> TermWs {
    let url = format!("ws://{addr}/api/sessions/{id}/terminal");
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("terminal websocket should connect");
    ws
}

/// A `0x00`-prefixed keystroke frame.
pub fn input_frame(s: &str) -> Vec<u8> {
    let mut v = vec![0x00u8];
    v.extend_from_slice(s.as_bytes());
    v
}

/// A `0x01 <cols_be> <rows_be>` resize frame.
pub fn resize_frame(cols: u16, rows: u16) -> Vec<u8> {
    let mut v = vec![0x01u8];
    v.extend_from_slice(&cols.to_be_bytes());
    v.extend_from_slice(&rows.to_be_bytes());
    v
}

/// Send a keystroke frame down a terminal WebSocket.
pub async fn send_input(ws: &mut TermWs, s: &str) {
    ws.send(Message::Binary(input_frame(s).into()))
        .await
        .unwrap();
}

/// Accumulate ALL binary output frames into one buffer (the marker may span
/// frames and is interleaved with ANSI escapes) until `marker` appears or the
/// timeout elapses. Returns the decoded buffer either way.
pub async fn drain_until(ws: &mut TermWs, marker: &str, timeout: Duration) -> String {
    use futures_util::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                buf.extend_from_slice(&b);
                if String::from_utf8_lossy(&buf).contains(marker) {
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}                 // text/ping/pong/close
            Ok(Some(Err(_))) | Ok(None) => break, // stream error / end
            Err(_) => break,                      // timeout
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}
