//! The operator scratch shell: `/api/shell/terminal` lazily spawns a single
//! login shell, keystrokes round-trip through it, and `POST /api/shell/restart`
//! replaces it with a fresh supervisor.

use std::time::Duration;

use futures_util::SinkExt;
use serde_json::json;
use serial_test::serial;
use tokio_tungstenite::tungstenite::Message;

use loom::backend;
use loom::shell::{self, SHELL_SESSION};

use crate::fixtures::{drain_until, send_input, TermWs, TestServer};

async fn connect_shell(addr: &std::net::SocketAddr) -> TermWs {
    let url = format!("ws://{addr}/api/shell/terminal");
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("shell websocket should connect");
    ws
}

/// Connect to a session's worktree debug shell `idx`.
async fn connect_session_shell(addr: &std::net::SocketAddr, id: &str, idx: u32) -> TermWs {
    let url = format!("ws://{addr}/api/sessions/{id}/shell/{idx}/terminal");
    let (ws, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("session shell websocket should connect");
    ws
}

/// Type an arithmetic marker until its *output* (distinct from the echoed
/// keystrokes) round-trips, proving the shell is up and executing.
async fn await_shell_ready(term: &mut TermWs, marker: &str) {
    let cmd = format!("echo {marker}$((6 * 7))\n");
    let want = format!("{marker}42");
    for _ in 0..15 {
        send_input(term, &cmd).await;
        if drain_until(term, &want, Duration::from_secs(1))
            .await
            .contains(&want)
        {
            return;
        }
    }
    panic!("scratch shell never came up");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_spawns_lazily_runs_and_restarts() {
    let ts = TestServer::start().await;

    // No shell exists until the first attach.
    assert!(
        !backend::has_session(SHELL_SESSION).await,
        "shell must not exist before anyone connects"
    );

    // Connecting spawns it; a command runs end to end.
    let mut term = connect_shell(&ts.addr).await;
    await_shell_ready(&mut term, "RDY").await;
    assert!(
        backend::has_session(SHELL_SESSION).await,
        "attaching must spawn the shell supervisor"
    );

    send_input(&mut term, "echo SHELL_MARKER_123\n").await;
    let out = drain_until(&mut term, "SHELL_MARKER_123", Duration::from_secs(8)).await;
    assert!(
        out.contains("SHELL_MARKER_123"),
        "marker never appeared in shell output:\n{out}"
    );
    term.send(Message::Close(None)).await.ok();
    drop(term);

    // Reconnecting reattaches to the SAME shell (closing a socket only detaches).
    let mut alive = false;
    for _ in 0..20 {
        if backend::has_session(SHELL_SESSION).await {
            alive = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(alive, "closing the socket must not kill the shell");

    // Restart replaces the supervisor; a fresh attach still works.
    ts.client
        .post("/api/shell/restart", json!({}))
        .await
        .unwrap();
    let mut term2 = connect_shell(&ts.addr).await;
    await_shell_ready(&mut term2, "RDY2").await;
    send_input(&mut term2, "echo SHELL_MARKER_456\n").await;
    let out2 = drain_until(&mut term2, "SHELL_MARKER_456", Duration::from_secs(8)).await;
    assert!(
        out2.contains("SHELL_MARKER_456"),
        "restarted shell never echoed:\n{out2}"
    );
    term2.send(Message::Close(None)).await.ok();

    // Best-effort cleanup so the detached supervisor doesn't linger.
    backend::kill_session(SHELL_SESSION).await.ok();
}

/// A session's worktree debug shells: each spawns lazily in the session's
/// worktree (carrying its `WEAVER_BRANCH`), `/shells` lists the live ones for
/// reload-rediscovery, and archiving the session sweeps every one.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_debug_shells_run_in_worktree_and_are_swept_on_archive() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let session = client
        .post(
            "/api/sessions",
            json!({ "goal": "debug shell test", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap().to_string();
    let work_dir = session["work_dir"].as_str().unwrap().to_string();

    // No debug shell exists until the first attach.
    assert!(
        shell::list_debug(&id).await.is_empty(),
        "no debug shells before anyone connects"
    );

    // Attaching to index 0 spawns a shell rooted in the worktree, with the
    // session's branch exported — distinct from the operator scratch shell.
    let mut sh0 = connect_session_shell(&ts.addr, &id, 0).await;
    await_shell_ready(&mut sh0, "RDY0").await;
    // `WT$((0))` is `WT0` in the OUTPUT but `WT$((0))` in the echoed keystrokes,
    // so draining for `WT0=` matches the command's result, not the typed line.
    send_input(&mut sh0, "echo WT$((0))=$PWD BR=${WEAVER_BRANCH:-none}\n").await;
    let probe = drain_until(&mut sh0, "WT0=", Duration::from_secs(8)).await;
    assert!(
        probe.contains(&format!("WT0={work_dir}")),
        "shell should land in the session's worktree; got:\n{probe}"
    );
    assert!(
        !probe.contains("BR=none"),
        "shell should export the session's WEAVER_BRANCH; got:\n{probe}"
    );

    // The live shell is rediscoverable both via the helper and the HTTP route.
    assert_eq!(shell::list_debug(&id).await, vec![0]);
    let listed = client
        .get(&format!("/api/sessions/{id}/shells"))
        .await
        .unwrap();
    assert_eq!(listed, json!([0]), "the route lists the live shell index");

    // A second index is an independent shell — multiple tabs.
    let mut sh1 = connect_session_shell(&ts.addr, &id, 1).await;
    await_shell_ready(&mut sh1, "RDY1").await;
    assert_eq!(shell::list_debug(&id).await, vec![0, 1], "both shells live");

    // Closing one tab (DELETE) kills just that supervisor.
    client
        .delete(&format!("/api/sessions/{id}/shell/0"))
        .await
        .unwrap();
    let mut gone = false;
    for _ in 0..20 {
        if !backend::has_session(&shell::debug_session(&id, 0)).await {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "closing a tab must kill that debug shell");
    assert_eq!(
        shell::list_debug(&id).await,
        vec![1],
        "the other shell lives"
    );

    // Archiving the session sweeps every remaining debug shell — a worktree
    // shell must never outlive the worktree it sits in.
    sh0.send(Message::Close(None)).await.ok();
    sh1.send(Message::Close(None)).await.ok();
    client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .unwrap();
    let mut swept = false;
    for _ in 0..20 {
        if shell::list_debug(&id).await.is_empty() {
            swept = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(swept, "archive must sweep every debug shell");
}
