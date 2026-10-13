//! Suspending a session stops its runtime — freeing its memory — while the
//! session row, worktree, and branch stay in place; waking restarts the runtime
//! and resumes the agent. The pair is the light alternative to archive/recover:
//! nothing is rebuilt, and every wake signal (a sent message, the Wake button)
//! brings a dormant session back in seconds.

use std::path::Path;
use std::time::Duration;

use futures_util::SinkExt;
use serde_json::json;
use serial_test::serial;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::Message;

use loom::backend;

use crate::fixtures::{drain_until, send_input, TestServer};

async fn launch_shell(ts: &TestServer, goal: &str) -> serde_json::Value {
    ts.client
        .post(
            "/api/sessions/launch",
            json!({ "goal": goal, "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap()
}

/// Plant the durable trace of an operation that died mid-transition, mirroring
/// the recover suite's fixture.
async fn plant_abandoned_transition(ts: &TestServer, id: &str, transition: &str, step: &str) {
    assert!(
        loom::session::begin_transition(&ts.state.db, id, transition, step)
            .await
            .unwrap()
    );
    sqlx::query("UPDATE sessions SET lifecycle_transition_owner_pid = ? WHERE id = ?")
        .bind(i64::from(i32::MAX))
        .bind(id)
        .execute(&ts.state.db)
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspend_stops_runtime_and_wake_resumes_in_place() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "suspend me").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();
    let branch = sess["branch"]["branch"].as_str().unwrap().to_string();
    assert!(backend::has_session(&term_session).await);

    // Suspend: the runtime stops, everything durable stays.
    let res = client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(res["suspended"], true);
    assert!(res["warnings"].as_array().unwrap().is_empty(), "{res}");
    assert!(
        !backend::has_session(&term_session).await,
        "suspend should stop the terminal session"
    );
    assert!(
        Path::new(&work_dir).exists(),
        "suspend must keep the worktree on disk"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended");
    assert_eq!(view["work_dir"], work_dir);
    assert_eq!(view["term_session"], term_session);
    assert!(
        weaver_core::git::branch_exists(ts.repo_path(), &branch).await,
        "suspend must keep the branch"
    );

    // A suspended session stays suspended — the monitor's liveness walk must
    // not read the deliberately stopped runtime as an orphan.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended");

    // Wake: the runtime comes back at the same worktree and terminal name.
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running");
    assert_eq!(woken["work_dir"], work_dir);
    assert_eq!(woken["term_session"], term_session);
    assert!(
        backend::has_session(&term_session).await,
        "wake should recreate the terminal session"
    );

    // The woken session is driveable again.
    let sent = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "echo back", "submit": true }),
        )
        .await
        .unwrap();
    assert_eq!(sent["sent"], true);

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sending_to_a_suspended_session_wakes_it() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "wake on send").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert!(!backend::has_session(&term_session).await);

    // A sent message is a wake signal: the runtime comes back first, then the
    // text lands in it.
    let sent = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "echo woken", "submit": true }),
        )
        .await
        .unwrap();
    assert_eq!(sent["sent"], true);
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running");
    assert!(backend::has_session(&term_session).await);

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspend_is_idempotent_and_refuses_terminal_sessions() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "suspend edge cases").await;
    let id = sess["id"].as_str().unwrap().to_string();

    // A second suspend of a suspended session is a no-op success.
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    let again = client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(again["suspended"], true);

    // Waking a session that is not suspended passes it through unchanged.
    client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running");

    // The retention ladder's tail: a suspended session remains an archive
    // candidate, and archiving one works — the already-stopped runtime is a
    // no-op kill.
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    let archived = client
        .post("/api/sessions/archive", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(archived["archived"], true);

    // A terminal session cannot be suspended — archive has already retired it.
    let error = client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("archived"), "{error}");
}

/// Opening a debug shell is an explicit reach for the runtime — a worktree shell
/// is derived from the agent's supervisor — so the attach wakes a suspended
/// session first, then spawns the shell against the revived supervisor.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_a_shell_wakes_a_suspended_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "wake me by opening a shell").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert!(
        !backend::has_session(&term_session).await,
        "suspend should stop the runtime the shell derives from"
    );

    // The websocket handshake runs the wake before upgrading: by the time the
    // client is connected, the session is live again.
    let url = format!(
        "ws://{}/api/sessions/shells/terminal?session={id}&index=0",
        ts.addr
    );
    let (mut shell_ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(
        view["status"], "running",
        "the shell attach wakes the session"
    );
    assert!(
        backend::has_session(&term_session).await,
        "the agent supervisor the shell derives from is back"
    );

    // The shell is live and lands in the session's worktree.
    send_input(&mut shell_ws, "echo SHWAKE$((2 * 21))\n").await;
    let out = drain_until(&mut shell_ws, "SHWAKE42", Duration::from_secs(8)).await;
    assert!(
        out.contains("SHWAKE42"),
        "shell never came up; output:\n{out}"
    );
    shell_ws.send(Message::Close(None)).await.ok();

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// An interrupted `suspending` whose owner died is reconciled on the monitor's
/// cadence: a surviving supervisor means the kill never happened (the session
/// stays live), a missing one means it did (the row completes to `suspended`).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciliation_settles_abandoned_suspending_and_waking_markers() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Alive supervisor: the interrupted suspend releases back to the live row.
    let sess = launch_shell(&ts, "stuck suspending").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    plant_abandoned_transition(&ts, &id, "suspending", "Stopping agent").await;

    loom::lifecycle::reconcile_interrupted_transitions(&ts.state).await;

    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running");
    assert!(view["transition"].is_null(), "{view}");
    assert!(backend::has_session(&term_session).await);

    // Dead supervisor: the interrupted suspend completes to `suspended`...
    backend::kill_session(&term_session).await.unwrap();
    plant_abandoned_transition(&ts, &id, "suspending", "Stopping agent").await;
    loom::lifecycle::reconcile_interrupted_transitions(&ts.state).await;
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended");
    assert!(view["transition"].is_null(), "{view}");

    // ...and an interrupted wake whose respawn never happened stays suspended
    // rather than leaving the row undriveable.
    plant_abandoned_transition(&ts, &id, "waking", "Restarting session runtime").await;
    loom::lifecycle::reconcile_interrupted_transitions(&ts.state).await;
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended");
    assert!(view["transition"].is_null(), "{view}");

    // A later wake still works on the reconciled row.
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// A wake runs to completion even when the request that started it goes away:
/// the locked body is a server-owned task, so a dropped client cannot cancel a
/// respawn whose `waking` marker is already published (this process owns the
/// marker, so no reconciliation would ever release it).
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_survives_request_disconnect() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "wake me then hang up").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert!(!backend::has_session(&term_session).await);

    // A raw connection lets the test close the transport without waiting for
    // the response — a browser navigation mid-wake.
    let mut connection = tokio::net::TcpStream::connect(ts.addr).await.unwrap();
    let body = json!({ "session": id }).to_string();
    let request = format!(
        "POST /api/sessions/wake HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        ts.addr,
        body.len(),
        body
    );
    connection.write_all(request.as_bytes()).await.unwrap();
    connection.flush().await.unwrap();

    // Wait until the wake has published its marker, then abandon the request.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let session = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if session.lifecycle_transition.as_deref() == Some("waking") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("wake should start");

    drop(connection);

    // The respawn finishes on its own: the row reaches `running` with a live
    // supervisor and no marker, instead of being stranded mid-wake.
    let woken = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let session = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if session.status == "running" && session.lifecycle_transition.is_none() {
                break session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wake should finish after its request disconnects");
    assert_eq!(woken.status, "running");
    assert!(
        backend::has_session(&term_session).await,
        "the supervisor should be back despite the dropped request"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// Typing into a terminal that a suspend is tearing down 409s instead of
/// pasting into the dying supervisor and silently losing the message.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sending_into_a_suspending_terminal_is_refused() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "do not paste while I suspend").await;
    let id = sess["id"].as_str().unwrap().to_string();

    assert!(
        loom::session::begin_transition(&ts.state.db, &id, "suspending", "Stopping agent")
            .await
            .unwrap()
    );
    let error = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "too late", "submit": true }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("suspending"), "{error}");

    loom::session::clear_transition(&ts.state.db, &id, "suspending")
        .await
        .unwrap();
    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// The reachable-but-rare state a failed kill leaves behind — the row says
/// `suspended` while the supervisor survived — must not strand the session:
/// wake settles it back to running instead of refusing.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_settles_a_suspended_row_whose_terminal_never_stopped() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "unstoppable").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    assert!(backend::has_session(&term_session).await);

    // Recreate the state directly: a suspend whose kill could only be warned
    // about commits `suspended` with the supervisor still alive.
    client
        .post(
            "/api/sessions/update",
            json!({ "session": id, "status": "suspended" }),
        )
        .await
        .unwrap();

    // Wake does not refuse on the live terminal — the goal is already met, so
    // the row settles back to running.
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running", "{woken}");
    assert!(backend::has_session(&term_session).await);

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// Every operator verb that brings a runtime back — adopt, wake, recover,
/// handoff — refreshes the idle anchor. Without that, a long-idle session
/// adopted or woken reads as stale on the monitor's very next tick and is
/// suspended again almost immediately. Backdate the anchor, bring the runtime
/// back, and require the anchor to have moved.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bringing_a_runtime_back_refreshes_the_idle_anchor() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let backdate = "2020-01-01T00:00:00.000Z";

    // Adopt: an orphaned session idle for years comes back without the reaper
    // immediately seeing it as stale.
    let sess = launch_shell(&ts, "adopted after ages idle").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    sqlx::query("UPDATE sessions SET last_activity_at = ? WHERE id = ?")
        .bind(backdate)
        .bind(&id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    backend::kill_session(&term_session).await.unwrap();
    let adopted = client
        .post("/api/sessions/adopt", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(adopted["status"], "running", "{adopted}");
    let session = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        session.last_activity_at.as_deref(),
        Some(backdate),
        "adopt must count as activity, or the next tick re-suspends"
    );

    // Wake: the same refresh applies when a long-dormant session is woken.
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET last_activity_at = ? WHERE id = ?")
        .bind(backdate)
        .bind(&id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running", "{woken}");
    let session = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        session.last_activity_at.as_deref(),
        Some(backdate),
        "wake must count as activity, or the next tick re-suspends"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}
