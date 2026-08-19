//! Driving a session's terminal over REST: `send` types + submits a line,
//! `preview` reads the screen back, `interrupt` injects a break, and all three
//! refuse a session whose terminal is gone. These drive the supervisor's control
//! socket (send / capture, no interactive PTY attach), so unlike the terminal
//! WebSocket suite they run everywhere.

use std::time::Duration;

use serde_json::json;
use serial_test::serial;

use loom::backend;

use crate::fixtures::TestServer;

/// Submit `text` and poll `sessions.preview` until the captured screen contains
/// `marker`, **re-submitting** between polls. The launch script `exec`s the shell
/// only after the supervisor socket is already up, and shell startup flushes any
/// input typed during that window — so a command sent right after create can be
/// echoed but never run. Re-submitting steps past that startup window; once the
/// shell is reading, the command executes and the marker appears.
async fn submit_until(ts: &TestServer, id: &str, text: &str, marker: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        // Poll the current screen for a short window before re-submitting.
        let inner = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let res = ts
                .client
                .post("/api/sessions/preview", json!({ "session": id }))
                .await
                .unwrap();
            let screen = res["screen"].as_str().unwrap_or("").to_string();
            if screen.contains(marker) {
                return screen;
            }
            if tokio::time::Instant::now() >= inner {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("marker {marker:?} never appeared in the pane after re-submitting");
        }
        // Not yet — (re)submit. Harmless if the earlier submit already ran.
        let _ = ts
            .client
            .post("/api/sessions/send", json!({ "session": id, "text": text }))
            .await;
    }
}

/// `send` (submit) runs a command in the shell; `preview` reads its output back.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_runs_a_command_and_preview_reads_it() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "pane test", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();

    // Submit a command whose OUTPUT (the arithmetic result) differs from the
    // text typed — so finding it proves the line was actually executed, not just
    // echoed onto the prompt.
    let sent = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "echo PANE_$((6 * 7))" }),
        )
        .await
        .unwrap();
    assert_eq!(sent["submitted"], true, "submit defaults to true");

    let screen = submit_until(&ts, &id, "echo PANE_$((6 * 7))", "PANE_42").await;
    assert!(screen.contains("PANE_42"), "command output missing");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// `send` with `submit:false` stages input without running it.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_without_submit_does_not_execute() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "pane test", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();

    let sent = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "echo STAGED_$((1 + 1))", "submit": false }),
        )
        .await
        .unwrap();
    assert_eq!(sent["submitted"], false);

    // Give the pane a beat, then confirm the arithmetic never ran: the literal
    // text is on the prompt line, but the evaluated `STAGED_2` is not.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let res = client
        .post("/api/sessions/preview", json!({ "session": id }))
        .await
        .unwrap();
    let screen = res["screen"].as_str().unwrap_or("");
    assert!(
        !screen.contains("STAGED_2"),
        "unsubmitted input should not have executed; screen:\n{screen}"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// `interrupt` injects an Escape and reports success.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_sends_a_break() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "pane test", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();

    let res = client
        .post("/api/sessions/interrupt", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(res["interrupted"], true);

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// All three pane endpoints 409 when the session has no live terminal.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_endpoints_reject_a_dead_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let ws = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "pane test", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();
    let session = ws["term_session"].as_str().unwrap().to_string();

    // Kill the terminal out from under loom — the session is now orphaned.
    backend::kill_session(&session).await.unwrap();
    assert!(!backend::has_session(&session).await);

    assert!(
        client
            .post(
                "/api/sessions/send",
                json!({ "session": id, "text": "echo hi" })
            )
            .await
            .is_err(),
        "send should fail without a live terminal"
    );
    assert!(
        client
            .post("/api/sessions/interrupt", json!({ "session": id }))
            .await
            .is_err(),
        "interrupt should fail without a live terminal"
    );
    assert!(
        client
            .post("/api/sessions/preview", json!({ "session": id }))
            .await
            .is_err(),
        "preview should fail without a live terminal"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}
