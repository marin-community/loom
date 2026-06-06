//! The terminal WebSocket: keystrokes reach the PTY and output round-trips, a
//! resize propagates, a large burst survives backpressure, and disconnecting
//! kills only the attach client — not the tmux session.

use std::time::Duration;

use futures_util::SinkExt;
use serde_json::json;
use serial_test::serial;
use tokio_tungstenite::tungstenite::Message;

use loom::tmux;

use crate::fixtures::{connect_terminal, drain_until, resize_frame, send_input, TestServer};

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_websocket_roundtrip() {
    // This test drives a private tmux server over a PTY, which doesn't come up
    // reliably on sandboxed CI runners. Skip it there (GitHub Actions sets
    // `CI=true`); it still runs in local dev where `git` + `tmux` are present.
    // The other integration suites don't touch a PTY, so they run everywhere.
    if std::env::var_os("CI").is_some() {
        eprintln!("skipping terminal_websocket_roundtrip: tmux PTY unavailable under CI");
        return;
    }

    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions",
            json!({
                "goal": "terminal test",
                "cwd": ts.cwd(),
                "agent": "shell",
            }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let session = ws["tmux_session"].as_str().unwrap().to_string();

    let mut term = connect_terminal(&ts.addr, &id).await;

    // Drive a real size, give tmux a moment to apply the SIGWINCH, then run a
    // command that prints the terminal width. The 0x01 → master.resize →
    // SIGWINCH → tmux pane path must reach the shell (width is unaffected by the
    // status line, unlike height).
    term.send(Message::Binary(resize_frame(120, 40).into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_input(&mut term, "echo DIMS COLS=$(tput cols)\n").await;
    let dims = drain_until(&mut term, "COLS=120", Duration::from_secs(8)).await;
    assert!(
        dims.contains("COLS=120"),
        "resize did not propagate to the pty width; got:\n{dims}"
    );

    // Output round-trip (the echoed keystrokes alone prove input→PTY→output).
    send_input(&mut term, "echo WS_MARKER_123\n").await;
    let out = drain_until(&mut term, "WS_MARKER_123", Duration::from_secs(8)).await;
    assert!(
        out.contains("WS_MARKER_123"),
        "marker never appeared in terminal output:\n{out}"
    );

    // Backpressure: a large burst must arrive without truncation/deadlock.
    // "line_5000" appears only in the OUTPUT, never in the typed command.
    send_input(&mut term, "for i in $(seq 1 5000); do echo line_$i; done\n").await;
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
    let mut term2 = connect_terminal(&ts.addr, &id).await;
    send_input(&mut term2, "echo WS_MARKER_456\n").await;
    let out2 = drain_until(&mut term2, "WS_MARKER_456", Duration::from_secs(8)).await;
    assert!(
        out2.contains("WS_MARKER_456"),
        "reconnected terminal never echoed:\n{out2}"
    );
    term2.send(Message::Close(None)).await.ok();

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}
