//! Suspending a session stops its runtime — freeing its memory — while the
//! session row, worktree, and branch stay in place; waking restarts the runtime
//! and resumes the agent. The pair is the light alternative to archive/recover:
//! nothing is rebuilt, and every wake signal (a sent message, the Wake button)
//! brings a dormant session back in seconds.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serial_test::serial;
use tokio::io::AsyncWriteExt;

use loom::backend;

use crate::fixtures::TestServer;

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
async fn sending_to_a_suspended_terminal_session_refuses_with_the_wake_remedy() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "wake me by hand").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert!(!backend::has_session(&term_session).await);

    // The uniform terminal rule: no queue, no auto-wake — a send refuses
    // with the remedy, no matter who is asking.
    let refused = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "echo woken", "submit": true }),
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("wake it first"),
        "the refusal must name the remedy: {refused}"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended", "a refused send must not wake");
    assert!(!backend::has_session(&term_session).await);

    // The manual wake is the way back, and the send then lands.
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running");
    let sent = client
        .post(
            "/api/sessions/send",
            json!({ "session": id, "text": "echo woken", "submit": true }),
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

/// Opening a debug shell on a suspended TERMINAL session refuses, per the
/// uniform rule — the shell derives from the agent's supervisor, which
/// dormancy stopped, and a dormant terminal row is woken only by the Wake
/// action.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_a_shell_on_a_suspended_terminal_session_refuses() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "shell me not").await;
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

    // The websocket handshake refuses without waking: the handshake
    // answer carries the remedy, and the row stays dormant.
    let url = format!(
        "ws://{}/api/sessions/shells/terminal?session={id}&index=0",
        ts.addr
    );
    let refused = tokio_tungstenite::connect_async(&url).await;
    assert!(
        refused.is_err(),
        "the shell attach must refuse on a dormant terminal session"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(
        view["status"], "suspended",
        "the shell attach must not wake"
    );
    assert!(!backend::has_session(&term_session).await);

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

    // Recreate the state directly (the update op refuses to fabricate
    // dormancy by design): a suspend whose kill could only be warned about
    // commits `suspended` with the supervisor still alive.
    sqlx::query("UPDATE sessions SET status = 'suspended' WHERE id = ?")
        .bind(&id)
        .execute(&ts.state.db)
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

/// A committed child result is a wake signal for its dormant parent: the
/// notice is durable in the parent's inbox, and the respawned parent's task
/// drains it. A duplicate post of the same idempotent result must not re-wake.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_result_wakes_a_suspended_parent() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // A real API-launched ACP parent under the scripted fake adapter, so the
    // session tree and channels all exist through the ordinary launch path.
    weaver_core::config::apply(
        &ts.state.db,
        &[(
            "acp.codex_cmd".to_string(),
            Some(crate::fixtures::fake_acp_agent_cmd()),
        )],
    )
    .await
    .unwrap();
    let parent = client
        .post(
            "/api/sessions/launch",
            json!({
                "title": "result parent", "goal": "say:ready", "cwd": ts.cwd(),
                "agent": "codex", "protocol": "acp",
            }),
        )
        .await
        .unwrap();
    assert_eq!(parent["protocol"], "acp", "{parent}");
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let parent_branch = parent["branch"]["id"].as_str().unwrap().to_string();

    // A child launched under the parent's session credential — that grant is
    // what links the two rows.
    let parent_row = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    let parent_token = loom::auth::create_session_token(
        &ts.state.db,
        parent_row.created_by.as_deref(),
        &parent_id,
        &parent_branch,
    )
    .await
    .unwrap();
    let http = reqwest::Client::new();
    let child: serde_json::Value = http
        .post(format!("http://{}/api/sessions/launch", ts.addr))
        .bearer_auth(&parent_token)
        .json(&json!({ "goal": "deliver a result", "cwd": ts.cwd(), "agent": "shell" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let child_token = loom::auth::create_session_token(
        &ts.state.db,
        None,
        &child_id,
        child["branch"]["id"].as_str().unwrap(),
    )
    .await
    .unwrap();

    // Wait out the parent's opening turn — suspend refuses mid-turn — then
    // park it.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, &parent_id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.acp_inflight.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("parent should settle after its opening turn");
    client
        .post("/api/sessions/suspend", json!({ "session": parent_id }))
        .await
        .unwrap();
    let parked = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parked.status, "suspended");

    // The child commits its result; the parent comes back for it.
    let posted = http
        .post(format!("http://{}/api/channels/messages/create", ts.addr))
        .bearer_auth(&child_token)
        .json(&json!({
            "channel": child_id, "kind": "result",
            "body": "the implementation is ready", "idempotency_key": "wake-parent-once",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(posted.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, &parent_id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.lifecycle_transition.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a committed child result should wake the suspended parent");

    // The drain of the result notice is itself a turn, dispatched during the
    // wake's startup — the parent's journal shows it as a session-update user
    // message. Wait until that turn has run and finished before parking the
    // parent again.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let chat = client
                .post("/api/sessions/chat", json!({ "session": parent_id }))
                .await
                .unwrap();
            let blocks = chat["blocks"].as_array().cloned().unwrap_or_default();
            let saw_notice = blocks.iter().any(|b| {
                b["kind"] == "user_message"
                    && b["payload"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("Update from loom session")
            });
            let turn_done = blocks.last().is_some_and(|b| b["kind"] == "turn_end");
            if saw_notice && turn_done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("parent should finish consuming the result notice");

    // A duplicate post of the same idempotent result must not re-wake: park
    // the parent again, re-post, and it stays dormant.
    client
        .post("/api/sessions/suspend", json!({ "session": parent_id }))
        .await
        .unwrap();
    let parked = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parked.status, "suspended");
    let reposted = http
        .post(format!("http://{}/api/channels/messages/create", ts.addr))
        .bearer_auth(&child_token)
        .json(&json!({
            "channel": child_id, "kind": "result",
            "body": "the implementation is ready", "idempotency_key": "wake-parent-once",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reposted.status(), reqwest::StatusCode::OK);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let still = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        still.status, "suspended",
        "a duplicate result post must not re-wake the parent"
    );

    client
        .post("/api/sessions/delete", json!({ "session": child_id }))
        .await
        .unwrap();
    client
        .post("/api/sessions/delete", json!({ "session": parent_id }))
        .await
        .unwrap();
}

/// The idle monitor's suspend hop, exercised directly: `auto_suspend` is the
/// one path between the monitor's verdict and `suspend_locked`, and it carries
/// the opt-out check nothing else reaches — the `auto-suspend: disabled` tag
/// must keep a running, idle-tagged session live.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_suspend_stops_the_runtime_and_the_opt_out_tag_blocks_it() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "auto suspend me").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();
    let (session, branch) = loom::session::with_branch(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();

    // The opt-out tag keeps the session running: no teardown, no state move.
    weaver_core::tags::set(
        &ts.state.db,
        &branch.id,
        weaver_core::tags::AUTO_SUSPEND_KEY,
        weaver_core::tags::AUTO_SUSPEND_DISABLED_VALUE,
        "",
        "test",
    )
    .await
    .unwrap();
    let skipped = loom::lifecycle::auto_suspend(
        &ts.state,
        &session,
        &branch,
        // The monitor's full verdict, minus the timing floor the direct call
        // bypasses anyway.
        |fresh| {
            fresh.status == "running"
                && fresh.lifecycle_transition.is_none()
                && fresh.managed_by.is_none()
                && fresh.acp_inflight.is_none()
        },
    )
    .await
    .unwrap();
    assert_eq!(skipped, None, "the opt-out tag must skip the suspend");
    assert!(backend::has_session(&term_session).await);
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running");

    // Without the tag the same idle session suspends through the shared path.
    weaver_core::tags::clear(
        &ts.state.db,
        &branch.id,
        weaver_core::tags::AUTO_SUSPEND_KEY,
    )
    .await
    .unwrap();
    let (session, branch) = loom::session::with_branch(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let warnings = loom::lifecycle::auto_suspend(&ts.state, &session, &branch, |fresh| {
        fresh.status == "running"
            && fresh.lifecycle_transition.is_none()
            && fresh.managed_by.is_none()
            && fresh.acp_inflight.is_none()
    })
    .await
    .unwrap()
    .expect("a running session should auto-suspend");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!backend::has_session(&term_session).await);
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// A channel delivery — not only a child result — wakes a dormant ACP
/// subscriber. ARCHITECTURE promises every ingress signal wakes; before the
/// ACP branch of `deliver_to_subscribers` only nudged an already-live task,
/// so a message posted to a suspended session's channel waited in its inbox
/// until somebody woke the session by hand.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_message_wakes_a_suspended_acp_session() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    weaver_core::config::apply(
        &ts.state.db,
        &[(
            "acp.codex_cmd".to_string(),
            Some(crate::fixtures::fake_acp_agent_cmd()),
        )],
    )
    .await
    .unwrap();
    let session = client
        .post(
            "/api/sessions/launch",
            json!({
                "title": "note target", "goal": "say:ready", "cwd": ts.cwd(),
                "agent": "codex", "protocol": "acp",
            }),
        )
        .await
        .unwrap();
    assert_eq!(session["protocol"], "acp", "{session}");
    let id = session["id"].as_str().unwrap().to_string();

    // Wait out the opening turn — suspend refuses mid-turn — then park it.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.acp_inflight.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("session should settle after its opening turn");
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();

    // A plain message lands on the dormant session's channel from outside —
    // a session never delivers to itself by design, so the sender here is
    // the operator, exactly as `loom channels send` would reach it.
    client
        .post(
            "/api/channels/messages/create",
            json!({ "channel": id, "kind": "message", "body": "wake up please" }),
        )
        .await
        .unwrap();

    // The delivery wakes the session...
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.lifecycle_transition.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a channel message should wake the suspended session");

    // ...and the respawned task drains the note.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let chat = client
                .post("/api/sessions/chat", json!({ "session": id }))
                .await
                .unwrap();
            let blocks = chat["blocks"].as_array().cloned().unwrap_or_default();
            let saw_note = blocks.iter().any(|b| {
                b["kind"] == "user_message"
                    && b["payload"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("wake up please")
            });
            let turn_done = blocks.last().is_some_and(|b| b["kind"] == "turn_end");
            if saw_note && turn_done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the woken session should consume the channel note");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// A wake whose respawn fails must leave the row in the cheap retryable state
/// — `suspended`, no stranded `waking` marker — so the next wake, once the
/// obstacle is gone, just works.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_wake_leaves_the_session_wakeable() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "fail my wake").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();

    // The obstacle: a missing worktree makes the respawn refuse.
    std::fs::remove_dir_all(&work_dir).unwrap();
    let failed = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await;
    assert!(
        failed.is_err(),
        "the wake should fail while the worktree is missing: {failed:?}"
    );
    let (row, _) = loom::session::with_branch(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "suspended");
    assert!(
        row.lifecycle_transition.is_none(),
        "a failed wake must release its marker: {row:?}"
    );

    // The obstacle gone, the same session wakes on the next try.
    std::fs::create_dir_all(&work_dir).unwrap();
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running", "{woken}");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// Dormancy is lifecycle-owned: a plain `sessions.update` can neither
/// fabricate `suspended` on a live session (a live runtime labelled dormant)
/// nor flip a suspended row to `running` (a dormant row that reads as an
/// orphan on the next tick). Those transitions live in the suspend/wake
/// verbs, teardown and all.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_status_update_cannot_fabricate_or_undo_dormancy() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = launch_shell(&ts, "keep your labels honest").await;
    let id = sess["id"].as_str().unwrap().to_string();

    let fabricated = client
        .post(
            "/api/sessions/update",
            json!({ "session": id, "status": "suspended" }),
        )
        .await;
    assert!(
        fabricated.is_err(),
        "fabricating dormancy must be rejected: {fabricated:?}"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "running", "{view}");

    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    let undone = client
        .post(
            "/api/sessions/update",
            json!({ "session": id, "status": "running" }),
        )
        .await;
    assert!(
        undone.is_err(),
        "undoing dormancy by label must be rejected: {undone:?}"
    );
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(view["status"], "suspended", "{view}");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// Shared scaffolding for the child-result wake contract tests: a fake-ACP
/// parent and a shell child launched under the parent's session credential.
async fn parent_and_child(
    ts: &TestServer,
    title: &str,
) -> (String, String, String, reqwest::Client, String) {
    weaver_core::config::apply(
        &ts.state.db,
        &[(
            "acp.codex_cmd".to_string(),
            Some(crate::fixtures::fake_acp_agent_cmd()),
        )],
    )
    .await
    .unwrap();
    let parent = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "title": title, "goal": "say:ready", "cwd": ts.cwd(),
                "agent": "codex", "protocol": "acp",
            }),
        )
        .await
        .unwrap();
    assert_eq!(parent["protocol"], "acp", "{parent}");
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let parent_branch = parent["branch"]["id"].as_str().unwrap().to_string();
    let parent_row = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    let parent_token = loom::auth::create_session_token(
        &ts.state.db,
        parent_row.created_by.as_deref(),
        &parent_id,
        &parent_branch,
    )
    .await
    .unwrap();
    let http = reqwest::Client::new();
    let child: serde_json::Value = http
        .post(format!("http://{}/api/sessions/launch", ts.addr))
        .bearer_auth(&parent_token)
        .json(&json!({ "goal": "deliver a result", "cwd": ts.cwd(), "agent": "shell" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let child_token = loom::auth::create_session_token(
        &ts.state.db,
        None,
        &child_id,
        child["branch"]["id"].as_str().unwrap(),
    )
    .await
    .unwrap();

    // Wait out the parent's opening turn — suspend refuses mid-turn.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, &parent_id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.acp_inflight.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("parent should settle after its opening turn");

    (parent_id, parent_branch, child_id, http, child_token)
}

/// Wait for the parent to leave `suspended`, or panic with `why`.
async fn await_running(ts: &TestServer, parent_id: &str, why: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, parent_id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.lifecycle_transition.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(why);
}

/// A child result that cannot wake its dormant parent — here a worktree the
/// respawn refuses — must not vanish: the retries exhaust, the parent stays
/// dormant with the result queued, and its branch carries a triage tag (never
/// the agent's own attention axis) saying a delivery waits. Removing the
/// obstacle and re-posting the same idempotent result recovers: the queued
/// notice re-wakes the parent, and the stale triage clears.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwakable_parent_is_triaged_instead_of_left_silent() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let (parent_id, parent_branch, child_id, http, child_token) =
        parent_and_child(&ts, "triaged parent").await;

    client
        .post("/api/sessions/suspend", json!({ "session": parent_id }))
        .await
        .unwrap();

    // The obstacle: a missing worktree, which the wake's respawn refuses
    // outright — fast, so the three retries exhaust quickly.
    let work_dir = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap()
        .work_dir;
    std::fs::remove_dir_all(&work_dir).unwrap();

    let posted = http
        .post(format!("http://{}/api/channels/messages/create", ts.addr))
        .bearer_auth(&child_token)
        .json(&json!({
            "channel": child_id, "kind": "result",
            "body": "the work is done", "idempotency_key": "triage-once",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(posted.status(), reqwest::StatusCode::OK);

    // The three retries (2s apart) exhaust and the triage tag appears.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let triage =
                weaver_core::tags::get(&ts.state.db, &parent_branch, weaver_core::tags::TRIAGE_KEY)
                    .await
                    .unwrap();
            if triage.is_some_and(|tag| tag.value == "attention") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("exhausted retries must raise the triage tag");

    // The parent stayed dormant, the result stayed queued, and the agent's
    // own attention axis was never touched.
    let row = loom::session::get(&ts.state.db, &parent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "suspended");
    let attention = weaver_core::tags::get(
        &ts.state.db,
        &parent_branch,
        weaver_core::tags::ATTENTION_KEY,
    )
    .await
    .unwrap();
    assert!(
        attention.is_none(),
        "the agent's own attention axis is never clobbered"
    );

    // The obstacle gone, the queued result's wake recovers the parent: a
    // re-post of the same idempotent result sees the notice still queued and
    // wakes it — and the recovery clears the stale triage the failure raised.
    std::fs::create_dir_all(&work_dir).unwrap();
    let reposted = http
        .post(format!("http://{}/api/channels/messages/create", ts.addr))
        .bearer_auth(&child_token)
        .json(&json!({
            "channel": child_id, "kind": "result",
            "body": "the work is done", "idempotency_key": "triage-once",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reposted.status(), reqwest::StatusCode::OK);
    await_running(
        &ts,
        &parent_id,
        "the recovered wake should bring the parent back",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let triage =
                weaver_core::tags::get(&ts.state.db, &parent_branch, weaver_core::tags::TRIAGE_KEY)
                    .await
                    .unwrap();
            if triage.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a successful wake must clear its own stale triage");

    client
        .post("/api/sessions/delete", json!({ "session": child_id }))
        .await
        .unwrap();
    client
        .post("/api/sessions/delete", json!({ "session": parent_id }))
        .await
        .unwrap();
}

/// The triage clear lives in `wake` itself, so a MANUAL wake — the
/// operator's wake, no delivery involved — clears a stale "delivery waiting" tag
/// too. And the clear is a compare-and-clear: a triage stamp that is not the
/// wake path's own survives it.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_wake_clears_the_stale_wake_triage_but_not_an_outside_assessment() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let sess = launch_shell(&ts, "triage recovery").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let branch = sess["branch"]["id"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    // The stamp a failed wake leaves behind.
    weaver_core::tags::set(
        &ts.state.db,
        &branch,
        weaver_core::tags::TRIAGE_KEY,
        "attention",
        "delivery waiting: the runtime did not come back",
        "system",
    )
    .await
    .unwrap();
    // The wake clears synchronously before its response, so the assertion
    // needs no polling.
    client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    let triage = weaver_core::tags::get(&ts.state.db, &branch, weaver_core::tags::TRIAGE_KEY)
        .await
        .unwrap();
    assert!(
        triage.is_none(),
        "a manual wake must clear its own stale triage: {triage:?}"
    );

    // But an assessment the wake path did not write is never clobbered.
    weaver_core::tags::set(
        &ts.state.db,
        &branch,
        weaver_core::tags::TRIAGE_KEY,
        "attention",
        "outside assessment: goal drifted from the work",
        "watch",
    )
    .await
    .unwrap();
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    let triage = weaver_core::tags::get(&ts.state.db, &branch, weaver_core::tags::TRIAGE_KEY)
        .await
        .unwrap();
    assert!(
        triage
            .as_ref()
            .is_some_and(|tag| tag.note.contains("outside assessment")),
        "a wake must not clear a triage it did not raise: {triage:?}"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// The uniform terminal rule on the publication path: a channel message to
/// a dormant terminal session publishes (the message is durable in channel
/// history) but never wakes the target and never queues a paste — the
/// receipt stays `queued`, the operator wakes the session, and a
/// re-publication of the same message is the retry.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_message_to_a_dormant_terminal_session_never_wakes_it() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let sess = launch_shell(&ts, "no wake for you").await;
    let id = sess["id"].as_str().unwrap().to_string();
    let term_session = sess["term_session"].as_str().unwrap().to_string();

    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    assert!(!backend::has_session(&term_session).await);

    let posted = client
        .post(
            "/api/channels/messages/create",
            json!({ "channel": id, "kind": "message", "body": "read me when you wake",
                    "idempotency_key": "no-wake-once" }),
        )
        .await
        .unwrap();
    let _ = posted;
    let view = client
        .post("/api/sessions/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(
        view["status"], "suspended",
        "a channel publication must not wake a dormant terminal session"
    );
    assert!(!backend::has_session(&term_session).await);
    let state: Option<String> = sqlx::query_scalar(
        "SELECT state FROM channel_deliveries cd
         JOIN channel_messages cm ON cm.id = cd.message_id
         WHERE cd.target_session_id = ? AND cm.body = ?",
    )
    .bind(&id)
    .bind("read me when you wake")
    .fetch_optional(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(
        state.as_deref(),
        Some("queued"),
        "the skipped delivery stays a plain queued receipt: {state:?}"
    );

    // Awake again, the same publication retries — the caller's retry is the
    // terminal lane's only retry — and lands.
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running");
    client
        .post(
            "/api/channels/messages/create",
            json!({ "channel": id, "kind": "message", "body": "read me when you wake",
                    "idempotency_key": "no-wake-once" }),
        )
        .await
        .unwrap();
    let delivered: Option<String> = sqlx::query_scalar(
        "SELECT state FROM channel_deliveries cd
         JOIN channel_messages cm ON cm.id = cd.message_id
         WHERE cd.target_session_id = ? AND cm.body = ?",
    )
    .bind(&id)
    .bind("read me when you wake")
    .fetch_optional(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(delivered.as_deref(), Some("delivered"));

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// A plain message to an ACP session whose suspend is still committing —
/// the driver live but stopping — is durably queued in the inbox, but the
/// mid-suspend wake it schedules is refused (the runtime is dying; the
/// refusal is the correct answer), so the session parks. The next wake —
/// here the operator's — is the retry, and the queued note drains.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_caught_mid_suspend_stays_queued_until_the_next_wake() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    weaver_core::config::apply(
        &ts.state.db,
        &[(
            "acp.codex_cmd".to_string(),
            Some(crate::fixtures::fake_acp_agent_cmd()),
        )],
    )
    .await
    .unwrap();
    let session = client
        .post(
            "/api/sessions/launch",
            json!({
                "title": "mid-suspend note target", "goal": "say:ready", "cwd": ts.cwd(),
                "agent": "codex", "protocol": "acp",
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap().to_string();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = loom::session::get(&ts.state.db, &id)
                .await
                .unwrap()
                .unwrap();
            if s.status == "running" && s.acp_inflight.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("session should settle after its opening turn");

    // A suspend caught mid-commit: the driver is live but the marker is
    // claimed, so the message cannot rely on the dying driver.
    assert!(loom::session::begin_transition(
        &ts.state.db,
        &id,
        "suspending",
        "Stopping session runtime",
    )
    .await
    .unwrap());
    client
        .post(
            "/api/channels/messages/create",
            json!({ "channel": id, "kind": "message", "body": "note while parking" }),
        )
        .await
        .unwrap();

    // Complete the suspend. The mid-suspend wake was refused — the runtime
    // it would have revived was dying — so the note waits in the inbox.
    tokio::time::sleep(Duration::from_secs(1)).await;
    sqlx::query(
        "UPDATE sessions SET lifecycle_transition = NULL, lifecycle_step = NULL, \
         lifecycle_transition_owner_pid = NULL, lifecycle_transition_started_at = NULL, \
         status = 'suspended' WHERE id = ?",
    )
    .bind(&id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    let row = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status, "suspended",
        "the dying runtime must not be revived"
    );
    let queued: Option<String> = sqlx::query_scalar(
        "SELECT ci.state FROM conversation_inbox ci
         JOIN channel_messages cm ON ci.delivery_key = 'channel:' || cm.id || ':' || ?
         WHERE cm.body = 'note while parking'",
    )
    .bind(&id)
    .fetch_optional(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(
        queued.as_deref(),
        Some("queued"),
        "the note must be durable in the inbox across the park: {queued:?}"
    );

    // The next wake is the retry: the session comes back and the inbox
    // drain lands the note.
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running", "{woken}");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state: Option<String> = sqlx::query_scalar(
                "SELECT ci.state FROM conversation_inbox ci
                 JOIN channel_messages cm ON ci.delivery_key = 'channel:' || cm.id || ':' || ?
                 WHERE cm.body = 'note while parking'",
            )
            .bind(&id)
            .fetch_optional(&ts.state.db)
            .await
            .unwrap();
            if state.as_deref() == Some("delivered") || state.as_deref() == Some("consumed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the woken task must drain the parked note");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// `sessions.wake` during a still-committing suspend is refused, not
/// silently "successful": the row reads `running` under the suspending
/// marker, but the suspend is about to contradict that answer.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_during_a_committing_suspend_is_refused() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let sess = launch_shell(&ts, "do not lie about my state").await;
    let id = sess["id"].as_str().unwrap().to_string();

    // A suspend caught mid-commit: the marker is claimed by this live
    // process, the runtime not yet stopped, the row still `running`.
    assert!(loom::session::begin_transition(
        &ts.state.db,
        &id,
        "suspending",
        "Stopping session runtime",
    )
    .await
    .unwrap());
    let refused = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await;
    assert!(
        refused.is_err(),
        "wake during a committing suspend must be refused: {refused:?}"
    );
    let row = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "running", "the refuse must not disturb the row");

    sqlx::query(
        "UPDATE sessions SET lifecycle_transition = NULL, lifecycle_step = NULL, \
         lifecycle_transition_owner_pid = NULL, lifecycle_transition_started_at = NULL \
         WHERE id = ?",
    )
    .bind(&id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// A wake that collides with a revival already in flight refuses cleanly
/// — "try again once it settles" — and the retry lands on the woken row.
/// The old behavior (wait out the concurrent wake on a 30s budget) is gone:
/// the refusal is the retry signal, and the seconds between are the
/// caller's to spend.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_during_another_wakes_marker_refuses_and_the_retry_lands() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let sess = launch_shell(&ts, "wake me twice").await;
    let id = sess["id"].as_str().unwrap().to_string();
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();

    // A revival in flight: the marker claimed by this still-live process.
    assert!(loom::session::begin_transition(
        &ts.state.db,
        &id,
        "waking",
        "Restarting session runtime",
    )
    .await
    .unwrap());
    let refused = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("try again once it settles"),
        "the refusal must name the retry: {refused}"
    );
    let row = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "suspended", "the refused wake must not act");

    // The revival settles; the retry lands on the woken row.
    loom::session::clear_transition(&ts.state.db, &id, "waking")
        .await
        .unwrap();
    let woken = client
        .post("/api/sessions/wake", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(woken["status"], "running", "{woken}");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

/// The delivery seam's contract: a clean running row passes through, a row
/// mid-lifecycle-transition is refused (pasting into a runtime being torn
/// down would ack the delivery and lose it), and a dormant TERMINAL row is
/// refused with the wake remedy — the uniform terminal rule, so no
/// programmatic ingress ever auto-wakes one.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_or_drivable_passes_running_refuses_transitions_and_dormant_terminal() {
    let ts = TestServer::start().await;
    let client = &ts.client;
    let sess = launch_shell(&ts, "drivable target").await;
    let id = sess["id"].as_str().unwrap().to_string();

    // A clean running row passes through unchanged.
    let clean = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let passed = loom::lifecycle::wake_or_drivable(&ts.state, &clean)
        .await
        .expect("a clean running row is drivable");
    assert_eq!(passed.status, "running");

    // A row mid-lifecycle-transition is refused — the paste would land in a
    // runtime being torn down or swapped underneath.
    assert!(loom::session::begin_transition(
        &ts.state.db,
        &id,
        "suspending",
        "Stopping session runtime",
    )
    .await
    .unwrap());
    let transitioning = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let refused = loom::lifecycle::wake_or_drivable(&ts.state, &transitioning).await;
    assert!(
        refused.is_err(),
        "a transitioning row must not be driven directly: {refused:?}"
    );
    sqlx::query(
        "UPDATE sessions SET lifecycle_transition = NULL, lifecycle_step = NULL, \
         lifecycle_transition_owner_pid = NULL, lifecycle_transition_started_at = NULL \
         WHERE id = ?",
    )
    .bind(&id)
    .execute(&ts.state.db)
    .await
    .unwrap();

    // A dormant terminal row refuses with the remedy, and stays dormant.
    client
        .post("/api/sessions/suspend", json!({ "session": id }))
        .await
        .unwrap();
    let dormant = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let refused = loom::lifecycle::wake_or_drivable(&ts.state, &dormant).await;
    let refused = refused.expect_err("the seam must refuse a dormant terminal row");
    let message = format!("{refused:#}");
    assert!(
        message.contains("wake it first"),
        "the refusal must name the remedy: {message}"
    );
    let row = loom::session::get(&ts.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status, "suspended",
        "the seam must not wake a terminal row"
    );

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}
