//! End-to-end test driving a real server with a shell-backed session.
//! Requires `git` and `tmux` on PATH.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use loom::client::Client;
use loom::events::EventBus;
use loom::web::AppState;
use loom::{db, server, tmux};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

/// Pins tmux to a throwaway server (`tmux -L <name>`) for the test and kills it
/// on drop — so the suite can never see or disturb the user's real sessions,
/// even if the test panics. See `loom::tmux::socket_args`.
struct TmuxSocket(String);

impl TmuxSocket {
    fn install() -> Self {
        let name = format!("weaver-test-{}", std::process::id());
        std::env::set_var("WEAVER_TMUX_SOCKET", &name);
        // Clear any stale server left by a crashed prior run with this pid.
        let _ = Command::new("tmux")
            .args(["-L", &name, "kill-server"])
            .status();
        TmuxSocket(name)
    }
}

impl Drop for TmuxSocket {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.0, "kill-server"])
            .status();
    }
}

type TermWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect a terminal WebSocket to a session. No `Origin` header is sent, so the
/// server's same-origin check takes the missing-Origin (non-browser) path.
async fn connect_terminal(addr: &SocketAddr, id: &str) -> TermWs {
    let url = format!("ws://{addr}/api/sessions/{id}/terminal");
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("terminal websocket should connect");
    ws
}

/// A `0x00`-prefixed keystroke frame.
fn input_frame(s: &str) -> Vec<u8> {
    let mut v = vec![0x00u8];
    v.extend_from_slice(s.as_bytes());
    v
}

/// A `0x01 <cols_be> <rows_be>` resize frame.
fn resize_frame(cols: u16, rows: u16) -> Vec<u8> {
    let mut v = vec![0x01u8];
    v.extend_from_slice(&cols.to_be_bytes());
    v.extend_from_slice(&rows.to_be_bytes());
    v
}

/// Accumulate ALL binary output frames into one buffer (the marker may span
/// frames and is interleaved with ANSI escapes) until `marker` appears or the
/// timeout elapses. Returns the decoded buffer either way.
async fn drain_until(ws: &mut TermWs, marker: &str, timeout: Duration) -> String {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_lifecycle() {
    // This test drives a private tmux server over a PTY, which doesn't come up
    // reliably on sandboxed CI runners. Skip it there (GitHub Actions sets
    // `CI=true`); it still runs in local dev where `git` + `tmux` are present.
    if std::env::var_os("CI").is_some() {
        eprintln!("skipping session_lifecycle: tmux PTY unavailable under CI");
        return;
    }

    // Isolate all weaver state in a temp dir.
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("WEAVER_HOME", home.path());
    // Isolate tmux onto a private server for the duration of the test.
    let _tmux = TmuxSocket::install();

    // Build a throwaway git repo with a single commit on `main`.
    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git", &["init", "-b", "main"]);
    sh(repo.path(), "git", &["config", "user.email", "t@t.test"]);
    sh(repo.path(), "git", &["config", "user.name", "Test"]);
    std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
    sh(repo.path(), "git", &["add", "."]);
    sh(repo.path(), "git", &["commit", "-m", "init"]);

    // Start the server on a random port.
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

    // Create a session backed by a plain shell.
    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "integration test goal",
                "cwd": repo.path().to_string_lossy(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let session = ws["tmux_session"].as_str().unwrap().to_string();
    let work_dir = ws["work_dir"].as_str().unwrap().to_string();
    let repo_root = ws["branch"]["repo_root"].as_str().unwrap().to_string();

    assert!(
        std::path::Path::new(&work_dir).join(".git").exists(),
        "worktree was not created"
    );
    assert!(
        work_dir.ends_with("/.worktrees/integration-test-goal"),
        "worktree should live inside the repo at .worktrees/<slug>, got {work_dir}"
    );
    assert_eq!(ws["branch"]["branch"], "weaver/integration-test-goal");
    assert_eq!(
        ws["branch"]["title"], "integration test goal",
        "title derived from goal"
    );
    assert!(tmux::has_session(&session).await, "tmux session missing");

    let list = client.get("/api/sessions").await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let recent = client.get("/api/repos/recent").await.unwrap();
    let recent = recent.as_array().unwrap();
    assert_eq!(
        recent.len(),
        1,
        "repo should be recorded after first session"
    );
    assert_eq!(recent[0]["repo_root"], repo_root);
    assert_eq!(recent[0]["active_branches"], 1);

    // ---- Terminal WebSocket --------------------------------------------------
    // Keystrokes reach the PTY and output round-trips; a resize propagates;
    // disconnecting kills only the attach client (not the session); a burst of
    // output survives the bounded-channel backpressure intact.
    {
        let mut term = connect_terminal(&addr, &id).await;

        // Drive a real size, give tmux a moment to apply the SIGWINCH, then run
        // a command that prints the terminal width. The 0x01 → master.resize →
        // SIGWINCH → tmux pane path must reach the shell (width is unaffected by
        // the status line, unlike height).
        term.send(Message::Binary(resize_frame(120, 40).into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        term.send(Message::Binary(
            input_frame("echo DIMS COLS=$(tput cols)\n").into(),
        ))
        .await
        .unwrap();
        let dims = drain_until(&mut term, "COLS=120", Duration::from_secs(8)).await;
        assert!(
            dims.contains("COLS=120"),
            "resize did not propagate to the pty width; got:\n{dims}"
        );

        // Output round-trip (the echoed keystrokes alone prove input→PTY→output).
        term.send(Message::Binary(input_frame("echo WS_MARKER_123\n").into()))
            .await
            .unwrap();
        let out = drain_until(&mut term, "WS_MARKER_123", Duration::from_secs(8)).await;
        assert!(
            out.contains("WS_MARKER_123"),
            "marker never appeared in terminal output:\n{out}"
        );

        // Backpressure: a large burst must arrive without truncation/deadlock.
        // "line_5000" appears only in the OUTPUT, never in the typed command.
        term.send(Message::Binary(
            input_frame("for i in $(seq 1 5000); do echo line_$i; done\n").into(),
        ))
        .await
        .unwrap();
        let burst = drain_until(&mut term, "line_5000", Duration::from_secs(20)).await;
        assert!(
            burst.contains("line_1") && burst.contains("line_5000"),
            "burst output was truncated under backpressure"
        );

        // Closing the socket must kill only the `tmux attach` client.
        term.send(Message::Close(None)).await.ok();
        drop(term);
        let mut alive = true;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            alive = tmux::has_session(&session).await;
            if !alive {
                break;
            }
        }
        assert!(alive, "closing the terminal must not kill the tmux session");

        // A second connection still works — proves attach-client-only teardown.
        let mut term2 = connect_terminal(&addr, &id).await;
        term2
            .send(Message::Binary(input_frame("echo WS_MARKER_456\n").into()))
            .await
            .unwrap();
        let out2 = drain_until(&mut term2, "WS_MARKER_456", Duration::from_secs(8)).await;
        assert!(
            out2.contains("WS_MARKER_456"),
            "reconnected terminal never echoed:\n{out2}"
        );
        term2.send(Message::Close(None)).await.ok();
    }

    // A hook flips the session status. The monitor consumes new `hook`
    // event rows on its next tick; any hook means the agent is alive → running.
    let branch_id = {
        let s = loom::session::get(&pool, &id).await.unwrap().unwrap();
        s.branch_id
    };
    weaver_core::events::record_local(&pool, &branch_id, "hook", json!({ "event": "working" }))
        .await
        .unwrap();
    let mut running = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ws = client.get(&format!("/api/sessions/{id}")).await.unwrap();
        if ws["status"] == "running" {
            running = true;
            break;
        }
    }
    assert!(running, "monitor should have flipped status to running");

    // Scratch files: empty to start, then an upload lands at scratch/<name> in
    // the worktree, is listed, and can be deleted. Path-traversal names are
    // rejected.
    {
        let scratch = client
            .get(&format!("/api/sessions/{id}/scratch"))
            .await
            .unwrap();
        assert_eq!(scratch.as_array().unwrap().len(), 0, "scratch starts empty");

        let http = reqwest::Client::new();
        let upload_url = format!("{}/api/sessions/{id}/scratch?name=notes.txt", client.base());
        let resp = http
            .post(&upload_url)
            .body("hello agent")
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "upload should succeed");

        // It physically exists under the worktree's scratch/ directory.
        let ws = client.get(&format!("/api/sessions/{id}")).await.unwrap();
        let work_dir = ws["work_dir"].as_str().unwrap();
        let dropped =
            std::fs::read_to_string(Path::new(work_dir).join("scratch/notes.txt")).unwrap();
        assert_eq!(dropped, "hello agent");

        let listed = client
            .get(&format!("/api/sessions/{id}/scratch"))
            .await
            .unwrap();
        let arr = listed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "notes.txt");
        assert_eq!(arr[0]["bytes"], 11);

        // Traversal attempts are refused.
        let bad = http
            .post(format!(
                "{}/api/sessions/{id}/scratch?name=../escape.txt",
                client.base()
            ))
            .body("nope")
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status().as_u16(), 400, "path traversal rejected");

        // Delete removes it.
        client
            .delete(&format!("/api/sessions/{id}/scratch?name=notes.txt"))
            .await
            .unwrap();
        let after = client
            .get(&format!("/api/sessions/{id}/scratch"))
            .await
            .unwrap();
        assert_eq!(
            after.as_array().unwrap().len(),
            0,
            "scratch empty after delete"
        );
    }

    // File viewer: the tree lists worktree files and badges changes vs base;
    // /file returns text content (working or base ref); /raw returns bytes; and
    // path traversal is refused just like scratch.
    {
        // Fresh worktree: README is listed and not yet changed.
        let tree = client
            .get(&format!("/api/sessions/{id}/tree"))
            .await
            .unwrap();
        let files: Vec<String> = tree["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            files.contains(&"README.md".to_string()),
            "tree lists README.md, got {files:?}"
        );
        assert!(
            !tree["changed"]
                .as_object()
                .unwrap()
                .contains_key("README.md"),
            "README unchanged before any edit"
        );

        let file = client
            .get(&format!("/api/sessions/{id}/file?path=README.md"))
            .await
            .unwrap();
        assert_eq!(file["content"], "hello\n");
        assert_eq!(file["binary"], false);

        // Edit a tracked file and drop a brand-new one.
        std::fs::write(Path::new(&work_dir).join("README.md"), "hello world\n").unwrap();
        std::fs::write(Path::new(&work_dir).join("new.txt"), "fresh\n").unwrap();

        let tree = client
            .get(&format!("/api/sessions/{id}/tree"))
            .await
            .unwrap();
        let changed = tree["changed"].as_object().unwrap();
        assert_eq!(changed["README.md"], "modified");
        assert_eq!(changed["new.txt"], "added", "untracked file shows as added");
        let files: Vec<String> = tree["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            files.contains(&"new.txt".to_string()),
            "untracked file listed in tree"
        );

        // base ref reads the merge-base version; working ref reads the edit.
        let base = client
            .get(&format!("/api/sessions/{id}/file?path=README.md&ref=base"))
            .await
            .unwrap();
        assert_eq!(
            base["content"], "hello\n",
            "base ref reads the merge-base version"
        );
        let work = client
            .get(&format!("/api/sessions/{id}/file?path=README.md"))
            .await
            .unwrap();
        assert_eq!(work["content"], "hello world\n");

        // Raw bytes carry the working-tree content.
        let http = reqwest::Client::new();
        let raw = http
            .get(format!(
                "{}/api/sessions/{id}/raw?path=new.txt",
                client.base()
            ))
            .send()
            .await
            .unwrap();
        assert!(raw.status().is_success());
        assert_eq!(raw.text().await.unwrap(), "fresh\n");

        // Traversal / absolute paths are refused on both reads.
        let bad = http
            .get(format!(
                "{}/api/sessions/{id}/file?path=../escape",
                client.base()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status().as_u16(), 400, "file path traversal rejected");
        let bad = http
            .get(format!(
                "{}/api/sessions/{id}/raw?path=/etc/passwd",
                client.base()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status().as_u16(), 400, "absolute raw path rejected");
    }

    // Branches endpoint lists this branch with the right metadata.
    let branches = client.get("/api/branches").await.unwrap();
    let arr = branches.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one branch tracked");
    assert_eq!(arr[0]["branch"], "weaver/integration-test-goal");
    assert_eq!(arr[0]["open_issue_count"], 0);

    // Branch issues are claimed by the branch; the repo-wide board lives at
    // /api/repos/issues.
    let created = client
        .post(
            &format!("/api/branches/{branch_id}/issues"),
            json!({ "title": "fix it", "body": "details" }),
        )
        .await
        .unwrap();
    let issue_id = created["id"].as_i64().unwrap();
    assert_eq!(created["status"], "open");
    assert_eq!(
        created["claimed_branch"], "weaver/integration-test-goal",
        "a branch issue is claimed by its branch"
    );
    let listed = client
        .get(&format!("/api/branches/{branch_id}/issues"))
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    let branch_view = client
        .get(&format!("/api/branches/{branch_id}"))
        .await
        .unwrap();
    assert_eq!(branch_view["open_issue_count"], 1);
    // The repo board sees the claimed issue; the unclaimed backlog does not.
    let board = client
        .get(&format!("/api/repos/issues?repo_root={repo_root}"))
        .await
        .unwrap();
    assert_eq!(board.as_array().unwrap().len(), 1);
    let backlog = client
        .get(&format!(
            "/api/repos/issues?repo_root={repo_root}&scope=backlog"
        ))
        .await
        .unwrap();
    assert_eq!(
        backlog.as_array().unwrap().len(),
        0,
        "issue is claimed, not backlog"
    );
    // Leave the issue in place: session teardown (below) must release it, not
    // delete it.

    // ---- Attach to an existing branch -----------------------------------
    let branches_q = client
        .get(&format!(
            "/api/repos/branches?cwd={}",
            repo.path().to_string_lossy()
        ))
        .await
        .unwrap();
    let arr = branches_q.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|b| b["name"] == "main" && b["current"] == true),
        "main should be listed as current, got {arr:?}"
    );

    sh(repo.path(), "git", &["branch", "feature/x", "main"]);
    let attached = client
        .post(
            "/api/sessions",
            json!({
                "cwd": repo.path().to_string_lossy(),
                "goal": "attach to feature/x",
                "agent": "shell",
                "existing_branch": "feature/x",
            }),
        )
        .await
        .unwrap();
    assert_eq!(attached["branch"]["branch"], "feature/x");
    let attached_id = attached["id"].as_str().unwrap().to_string();
    let attached_dir = attached["work_dir"].as_str().unwrap().to_string();
    assert!(
        attached_dir.ends_with("/.worktrees/feature-x"),
        "attached worktree should live at .worktrees/feature-x, got {attached_dir}"
    );
    assert!(std::path::Path::new(&attached_dir).join(".git").exists());

    sh(repo.path(), "git", &["branch", "feature/y", "main"]);
    let preexisting = repo.path().join("custom-worktree-y");
    sh(
        repo.path(),
        "git",
        &[
            "worktree",
            "add",
            preexisting.to_str().unwrap(),
            "feature/y",
        ],
    );
    let attached_y = client
        .post(
            "/api/sessions",
            json!({
                "cwd": repo.path().to_string_lossy(),
                "goal": "attach to feature/y",
                "agent": "shell",
                "existing_branch": "feature/y",
            }),
        )
        .await
        .unwrap();
    assert_eq!(attached_y["branch"]["branch"], "feature/y");
    let dir_y = attached_y["work_dir"].as_str().unwrap().to_string();
    assert_eq!(
        std::fs::canonicalize(&dir_y).unwrap(),
        std::fs::canonicalize(&preexisting).unwrap(),
        "weaver should reuse the pre-existing worktree path"
    );

    let missing = client
        .post(
            "/api/sessions",
            json!({
                "cwd": repo.path().to_string_lossy(),
                "goal": "missing branch",
                "agent": "shell",
                "existing_branch": "no/such/branch",
            }),
        )
        .await;
    assert!(missing.is_err(), "missing branch should be rejected");

    client
        .delete(&format!("/api/sessions/{attached_id}"))
        .await
        .unwrap();
    client
        .delete(&format!(
            "/api/sessions/{}",
            attached_y["id"].as_str().unwrap()
        ))
        .await
        .unwrap();

    // Adoption.
    tmux::kill_session(&session).await.unwrap();
    assert!(
        !tmux::has_session(&session).await,
        "session should be gone after kill"
    );
    let adopted = client
        .post(&format!("/api/sessions/{id}/adopt"), json!({}))
        .await
        .unwrap();
    assert_eq!(
        adopted["status"], "launching",
        "adopt sets status launching"
    );
    assert!(
        tmux::has_session(&session).await,
        "adopt should recreate the tmux session"
    );
    assert!(
        client
            .post(&format!("/api/sessions/{id}/adopt"), json!({}))
            .await
            .is_err(),
        "adopting a live session should fail"
    );

    // A session can be created with no goal at all.
    let bare = client
        .post(
            "/api/sessions",
            json!({
                "cwd": repo.path().to_string_lossy(),
                "title": "no goal here",
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    assert_eq!(bare["branch"]["goal"], "", "goal should be empty");
    assert_eq!(bare["branch"]["title"], "no goal here");
    let bare_id = bare["id"].as_str().unwrap().to_string();
    client
        .delete(&format!("/api/sessions/{bare_id}"))
        .await
        .unwrap();

    // ---- Archive -------------------------------------------------------------
    // Archiving tears down tmux + worktree but, unlike delete, keeps the session
    // row (marked `archived`), the git branch, and the weaver history.
    let arch = client
        .post(
            "/api/sessions",
            json!({
                "goal": "archive me",
                "cwd": repo.path().to_string_lossy(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let arch_id = arch["id"].as_str().unwrap().to_string();
    let arch_session = arch["tmux_session"].as_str().unwrap().to_string();
    let arch_work_dir = arch["work_dir"].as_str().unwrap().to_string();
    assert!(
        tmux::has_session(&arch_session).await,
        "archive session missing"
    );
    assert!(
        Path::new(&arch_work_dir).exists(),
        "archive worktree missing"
    );
    // A note proves the weaver history survives the archive.
    client
        .post(
            &format!("/api/sessions/{arch_id}/note"),
            json!({ "text": "decision: keep going" }),
        )
        .await
        .unwrap();

    let res = client
        .post(&format!("/api/sessions/{arch_id}/archive"), json!({}))
        .await
        .unwrap();
    assert_eq!(res["archived"], true);
    assert!(
        !tmux::has_session(&arch_session).await,
        "archive should kill the tmux session"
    );
    assert!(
        !Path::new(&arch_work_dir).exists(),
        "archive should remove the worktree"
    );
    // The session row persists, now terminal/`archived`.
    let view = client
        .get(&format!("/api/sessions/{arch_id}"))
        .await
        .unwrap();
    assert_eq!(view["status"], "archived");
    // The git branch is left intact for future reference.
    assert!(
        weaver_core::git::branch_exists(repo.path(), "weaver/archive-me").await,
        "archive must not delete the branch"
    );
    // The note history survives in the branch log.
    let log = client
        .get(&format!("/api/sessions/{arch_id}/log"))
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&log).unwrap().contains("keep going"),
        "note history should survive archive"
    );
    // An archived session can still be fully removed afterwards.
    client
        .delete(&format!("/api/sessions/{arch_id}"))
        .await
        .unwrap();

    // Deleting the session tears down the tmux session and the DB row.
    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
    assert!(
        !tmux::has_session(&session).await,
        "tmux session was not killed"
    );
    let list = client.get("/api/sessions").await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);

    // The claimed issue is repo-owned: it outlives its session, returning to
    // the unclaimed backlog rather than being deleted with the branch.
    let board = client
        .get(&format!("/api/repos/issues?repo_root={repo_root}&all=true"))
        .await
        .unwrap();
    let board = board.as_array().unwrap();
    assert_eq!(board.len(), 1, "issue survived teardown");
    assert_eq!(board[0]["id"].as_i64().unwrap(), issue_id);
    assert!(
        board[0]["claimed_branch"].is_null(),
        "claim was released on teardown"
    );

    let recent = client.get("/api/repos/recent").await.unwrap();
    let recent = recent.as_array().unwrap();
    assert_eq!(recent.len(), 1, "recent repo should outlive its sessions");
    assert_eq!(recent[0]["repo_root"], repo_root);
    assert_eq!(recent[0]["active_branches"], 0);
}
