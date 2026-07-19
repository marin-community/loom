//! The loom ACP client end to end: a real relay supervisor runs the scripted
//! `fake-acp-agent.mjs`, and `loom::acp` drives it over JSON-RPC while the HTTP
//! `/chat`, `/prompt`, `/permissions`, `/mode`, and `/interrupt` routes exercise
//! the same session. The suite shares the server's `AppState` (its ACP registry
//! is `Arc`-shared), so `loom::acp::start`/`attach` register into the very
//! registry the routes read.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use serial_test::serial;
use tokio::sync::broadcast;

use loom::acp::{self, AcpLaunch, NewOrLoad, SseEvent};
use loom::backend;
use loom::session::{self as session_mod, NewSession};

use crate::fixtures::{branch_tag_value, TestServer};

/// The relay command that launches the scripted fake ACP agent over stdio.
fn agent_cmd() -> String {
    crate::fixtures::fake_acp_agent_cmd()
}

/// Set an env var for the test's duration, restoring the prior value on drop.
struct EnvVarSet {
    name: &'static str,
    prev: Option<std::ffi::OsString>,
}
impl EnvVarSet {
    fn set(name: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, prev }
    }
}
impl Drop for EnvVarSet {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.name, v),
            None => std::env::remove_var(self.name),
        }
    }
}

/// Insert a fresh (branch, session) pair directly — the session row `acp::start`
/// binds a relay to. `term_session` doubles as the relay name.
async fn make_session(ts: &TestServer, id: &str) {
    let branch = loom::branch::upsert(&ts.state.db, &ts.cwd(), &format!("weaver/{id}"), "main")
        .await
        .unwrap();
    session_mod::insert(
        &ts.state.db,
        &NewSession {
            id: id.to_string(),
            branch_id: branch.id,
            work_dir: ts.cwd(),
            term_session: format!("weaver-{id}"),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: None,
            protocol: "acp".to_string(),
        },
    )
    .await
    .unwrap();
}

/// Bring up a fresh ACP session (relay + handshake + task) with the given launch
/// mode and optional goal.
async fn start_new(ts: &TestServer, id: &str, mode: Option<&str>, goal: Option<&str>) {
    make_session(ts, id).await;
    let cwd = ts.repo_path().to_path_buf();
    let launch = AcpLaunch {
        adapter_cmd: agent_cmd(),
        cwd: cwd.clone(),
        env: vec![],
        new_or_load: NewOrLoad::New { cwd, meta: None },
        mode: mode.map(str::to_string),
        goal: goal.map(str::to_string),
    };
    acp::start(&ts.state, id, launch)
        .await
        .expect("acp session starts");
}

/// Collect broadcast SSE events until `until` matches one (or the timeout).
async fn drain_events(
    rx: &mut broadcast::Receiver<SseEvent>,
    timeout: Duration,
    until: impl Fn(&SseEvent) -> bool,
) -> Vec<SseEvent> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                let stop = until(&ev);
                out.push(ev);
                if stop {
                    break;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    out
}

/// Poll `GET /chat` until `pred` accepts the block list, returning the chat body.
async fn poll_chat(
    ts: &TestServer,
    id: &str,
    timeout: Duration,
    pred: impl Fn(&[Value]) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let chat = ts
            .client
            .get(&format!("/api/sessions/{id}/chat"))
            .await
            .unwrap();
        let empty = vec![];
        let blocks = chat["blocks"].as_array().unwrap_or(&empty);
        if pred(blocks) {
            return chat;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("chat never satisfied the predicate; last: {chat}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn kinds(blocks: &[Value]) -> Vec<String> {
    blocks
        .iter()
        .map(|b| b["kind"].as_str().unwrap_or("").to_string())
        .collect()
}

fn count_kind(blocks: &[Value], kind: &str) -> usize {
    blocks.iter().filter(|b| b["kind"] == kind).count()
}

/// 1. New session end to end: prompt → journal has user_message + agent_message +
///    turn_end; SSE delivered delta + block + turn events.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_session_end_to_end() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-e2e", None, None).await;

    // Subscribe before prompting so no event is missed.
    let mut rx = ts
        .state
        .acp
        .get("acp-e2e")
        .expect("task registered")
        .subscribe();

    let res = ts
        .client
        .post(
            "/api/sessions/acp-e2e/prompt",
            json!({ "text": "say:hello" }),
        )
        .await
        .unwrap();
    assert_eq!(res["queued"], false, "an idle session dispatches at once");
    assert_eq!(res["turn"], 0);

    let events = drain_events(&mut rx, Duration::from_secs(10), |e| {
        e.event == "turn" && e.data["state"] == "ended"
    })
    .await;

    assert!(
        events
            .iter()
            .any(|e| e.event == "turn" && e.data["state"] == "started"),
        "a turn-started event"
    );
    assert!(
        events
            .iter()
            .any(|e| e.event == "delta" && e.data["kind"] == "agent_message"),
        "an agent_message delta streamed"
    );
    assert!(
        events.iter().any(|e| e.event == "block"
            && e.data["kind"] == "agent_message"
            && e.data["payload"]["text"] == "hello"),
        "a consolidated agent_message block"
    );
    assert!(
        events
            .iter()
            .any(|e| e.event == "block" && e.data["kind"] == "user_message"),
        "the user_message block"
    );
    assert!(
        events.iter().any(|e| e.event == "turn"
            && e.data["state"] == "ended"
            && e.data["stop_reason"] == "end_turn"),
        "a turn-ended event with end_turn"
    );

    let chat = ts.client.get("/api/sessions/acp-e2e/chat").await.unwrap();
    let blocks = chat["blocks"].as_array().unwrap();
    let ks = kinds(blocks);
    assert!(ks.contains(&"user_message".to_string()));
    assert!(ks.contains(&"agent_message".to_string()));
    assert!(ks.contains(&"turn_end".to_string()));
    assert_eq!(
        chat["live_turn"],
        Value::Null,
        "turn ended, nothing in flight"
    );

    // The SessionView exposes the ACP fields.
    let view = ts.client.get("/api/sessions/acp-e2e").await.unwrap();
    assert_eq!(view["protocol"], "acp");
    assert!(view["acp_session_id"]
        .as_str()
        .unwrap()
        .starts_with("fake-session-"));
}

/// 1b. The HTTP `/chat/stream` route streams the same events over SSE.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_stream_route_delivers_sse() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-sse", None, None).await;

    // Opening the stream subscribes the broadcast before we prompt.
    let url = format!("http://{}/api/sessions/acp-sse/chat/stream", ts.addr);
    let resp = reqwest::Client::new().get(&url).send().await.unwrap();
    assert!(resp.status().is_success());

    ts.client
        .post(
            "/api/sessions/acp-sse/prompt",
            json!({ "text": "say:streamed" }),
        )
        .await
        .unwrap();

    let mut stream = resp.bytes_stream();
    let mut body = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("\"state\":\"ended\"") {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        body.contains("event: turn"),
        "stream carried turn events: {body}"
    );
    assert!(
        body.contains("event: block"),
        "stream carried block events: {body}"
    );
    assert!(
        body.contains("event: delta"),
        "stream carried delta events: {body}"
    );
}

/// 2. Tool call: a live `tool` SSE, then one journaled `tool_call` block at a
///    terminal status carrying the diff content.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_call_live_then_journaled() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-tool", None, None).await;
    let mut rx = ts.state.acp.get("acp-tool").unwrap().subscribe();

    ts.client
        .post(
            "/api/sessions/acp-tool/prompt",
            json!({ "text": "tool:edit" }),
        )
        .await
        .unwrap();

    let events = drain_events(&mut rx, Duration::from_secs(10), |e| {
        e.event == "turn" && e.data["state"] == "ended"
    })
    .await;

    assert!(
        events.iter().any(|e| e.event == "tool"
            && e.data["status"] == "in_progress"
            && e.data["tool_kind"] == "edit"),
        "a live in-progress tool event"
    );
    let tool_blocks: Vec<&SseEvent> = events
        .iter()
        .filter(|e| e.event == "block" && e.data["kind"] == "tool_call")
        .collect();
    assert_eq!(
        tool_blocks.len(),
        1,
        "exactly one journaled tool_call block"
    );
    assert_eq!(tool_blocks[0].data["payload"]["status"], "completed");
    let content = tool_blocks[0].data["payload"]["content"]
        .as_array()
        .unwrap();
    assert!(
        content
            .iter()
            .any(|c| c["type"] == "diff" && c["new"] == "new line\n"),
        "the diff content survived: {content:?}"
    );

    let chat = ts.client.get("/api/sessions/acp-tool/chat").await.unwrap();
    let blocks = chat["blocks"].as_array().unwrap();
    assert_eq!(
        count_kind(blocks, "tool_call"),
        1,
        "one tool_call in the journal"
    );
}

/// 3a. Permission auto-answer: under `bypassPermissions` the request is answered
///     by policy and the turn completes without a REST call.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_auto_answered_under_bypass() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-auto", Some("bypassPermissions"), None).await;
    let mut rx = ts.state.acp.get("acp-auto").unwrap().subscribe();

    ts.client
        .post(
            "/api/sessions/acp-auto/prompt",
            json!({ "text": "permission:secret|say:done" }),
        )
        .await
        .unwrap();

    let events = drain_events(&mut rx, Duration::from_secs(10), |e| {
        e.event == "turn" && e.data["state"] == "ended"
    })
    .await;
    assert!(
        events.iter().any(|e| e.event == "turn"
            && e.data["state"] == "ended"
            && e.data["stop_reason"] == "end_turn"),
        "the turn completed after the auto-answer"
    );

    let chat = ts.client.get("/api/sessions/acp-auto/chat").await.unwrap();
    let blocks = chat["blocks"].as_array().unwrap();
    let perm = blocks
        .iter()
        .find(|b| b["kind"] == "permission_request")
        .expect("a permission_request block");
    assert_eq!(
        perm["payload"]["outcome"]["option_id"], "allow-once",
        "first allow chosen"
    );
    assert_eq!(
        perm["payload"]["outcome"]["by"], "policy",
        "answered by policy"
    );
    // The turn still reached `say:done`.
    assert!(
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "done"),
        "the turn continued past the permission"
    );
}

/// 3b. Permission REST-answer: under `default` the request stays open until a
///     `POST /permissions/{id}` answers it, then the turn completes.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_answered_over_rest() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-rest", Some("default"), None).await;

    ts.client
        .post(
            "/api/sessions/acp-rest/prompt",
            json!({ "text": "permission:edit-file|say:granted" }),
        )
        .await
        .unwrap();

    // The request surfaces as an open permission_request block (no auto-answer).
    let chat = poll_chat(&ts, "acp-rest", Duration::from_secs(10), |blocks| {
        blocks
            .iter()
            .any(|b| b["kind"] == "permission_request" && b["payload"]["outcome"].is_null())
    })
    .await;
    let request_id = chat["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["kind"] == "permission_request")
        .unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(chat["live_turn"], 0, "the turn is blocked, still in flight");

    // Answering an unknown id is a 404.
    assert!(
        ts.client
            .post(
                "/api/sessions/acp-rest/permissions/nope",
                json!({ "option_id": "allow-once" }),
            )
            .await
            .is_err(),
        "unknown request id 404s"
    );

    let res = ts
        .client
        .post(
            &format!("/api/sessions/acp-rest/permissions/{request_id}"),
            json!({ "option_id": "allow-once" }),
        )
        .await
        .unwrap();
    assert_eq!(res["resolved"], true);

    // The agent got the answer and the turn completed.
    let chat = poll_chat(&ts, "acp-rest", Duration::from_secs(10), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();
    let perm = blocks
        .iter()
        .find(|b| b["kind"] == "permission_request")
        .unwrap();
    assert_eq!(perm["payload"]["outcome"]["option_id"], "allow-once");
    assert_eq!(perm["payload"]["outcome"]["by"], "manual");

    // Answering again is a 409 (already resolved).
    assert!(
        ts.client
            .post(
                &format!("/api/sessions/acp-rest/permissions/{request_id}"),
                json!({ "option_id": "allow-once" }),
            )
            .await
            .is_err(),
        "a resolved request 409s"
    );
}

/// 4. Prompt queueing: a send during a live turn queues, sets `pending_prompt`,
///    and dispatches as a second turn once the first ends.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_queues_during_a_live_turn() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-queue", None, None).await;

    let first = ts
        .client
        .post(
            "/api/sessions/acp-queue/prompt",
            json!({ "text": "wait:700|say:first" }),
        )
        .await
        .unwrap();
    assert_eq!(first["queued"], false);
    assert_eq!(first["turn"], 0);

    // Give the first turn a moment to be in flight, then send again.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = ts
        .client
        .post(
            "/api/sessions/acp-queue/prompt",
            json!({ "text": "say:second" }),
        )
        .await
        .unwrap();
    assert_eq!(second["queued"], true, "a send during a turn queues");
    assert_eq!(second["turn"], 0, "queued against the live turn");

    let session = session_mod::get(&ts.state.db, "acp-queue")
        .await
        .unwrap()
        .unwrap();
    assert!(
        session
            .pending_prompt
            .as_deref()
            .unwrap_or("")
            .contains("say:second"),
        "the queue is persisted"
    );

    // The queued prompt dispatches as turn 1 once turn 0 ends.
    let chat = poll_chat(&ts, "acp-queue", Duration::from_secs(10), |blocks| {
        count_kind(blocks, "turn_end") >= 2
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();
    assert!(
        blocks.iter().any(|b| b["kind"] == "user_message"
            && b["turn"] == 1
            && b["payload"]["text"] == "say:second"),
        "the queued text became turn 1's user_message"
    );
    assert!(
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "second"),
        "the queued turn ran"
    );
}

/// 5. Crash recovery: stop the loom-side task mid-turn, re-attach, and the
///    replayed frames re-ingest with no duplicate blocks and an advanced cursor.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_recovery_replays_without_duplicates() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-crash", None, None).await;

    ts.client
        .post(
            "/api/sessions/acp-crash/prompt",
            json!({ "text": "say:recovered|wait:1500" }),
        )
        .await
        .unwrap();

    // Let the message chunks stream (buffered, not flushed) while the turn waits.
    tokio::time::sleep(Duration::from_millis(350)).await;
    let before = session_mod::get(&ts.state.db, "acp-crash")
        .await
        .unwrap()
        .unwrap();

    // "Crash" the loom-side task; the relay + agent survive.
    assert!(ts.state.acp.stop("acp-crash"), "a task was running");
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Re-attach: replay from the persisted cursor.
    acp::attach(&ts.state, "acp-crash")
        .await
        .expect("re-attach succeeds");

    let chat = poll_chat(&ts, "acp-crash", Duration::from_secs(10), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();

    // No duplicates: the UNIQUE(session,turn,seq) key held, and recovery did not
    // re-journal committed blocks at fresh seqs.
    assert_eq!(count_kind(blocks, "user_message"), 1, "one user_message");
    assert_eq!(
        count_kind(blocks, "agent_message"),
        1,
        "one agent_message (no dup)"
    );
    assert_eq!(count_kind(blocks, "turn_end"), 1, "one turn_end (no dup)");
    assert!(
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "recovered"),
        "the buffered message rebuilt intact"
    );

    // The ack persists just *after* the block it completes commits (block-boundary
    // acking: never ack a frame until its journal write lands), so the cursor
    // advance trails the visible `turn_end` block — poll for it to settle.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let after = loop {
        let s = session_mod::get(&ts.state.db, "acp-crash")
            .await
            .unwrap()
            .unwrap();
        if s.acp_ack_seq > before.acp_ack_seq || tokio::time::Instant::now() >= deadline {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        after.acp_ack_seq > before.acp_ack_seq,
        "the ack cursor advanced across recovery ({} -> {})",
        before.acp_ack_seq,
        after.acp_ack_seq
    );
}

/// 5b. Adapter user echoes never re-journal: a `user_message_chunk` streamed
///    mid-turn (claude re-streams retained user turns after `/compact`) must not
///    duplicate the history — the prompt loom journaled at dispatch is the only
///    `user_message` block.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_echo_chunks_do_not_duplicate_history() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-echo", None, None).await;

    // The adapter echoes two user turns (as after a /compact replay), then replies.
    let script = "echo:what is the PR status|echo:/compact|say:done";
    ts.client
        .post("/api/sessions/acp-echo/prompt", json!({ "text": script }))
        .await
        .unwrap();

    let chat = poll_chat(&ts, "acp-echo", Duration::from_secs(10), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();

    assert_eq!(
        count_kind(blocks, "user_message"),
        1,
        "only the dispatched prompt is a user_message: {blocks:?}"
    );
    assert!(
        blocks
            .iter()
            .any(|b| b["kind"] == "user_message" && b["payload"]["text"] == script),
        "and it is the prompt loom journaled at dispatch"
    );
    assert!(
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "done"),
        "the agent reply still journals"
    );
}

/// 6. Interrupt: cancelling a waiting turn ends it with stop reason `cancelled`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_cancels_the_turn() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-int", None, None).await;
    let mut rx = ts.state.acp.get("acp-int").unwrap().subscribe();

    ts.client
        .post(
            "/api/sessions/acp-int/prompt",
            json!({ "text": "wait:3000|say:unreached" }),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    let res = ts
        .client
        .post("/api/sessions/acp-int/interrupt", json!({}))
        .await
        .unwrap();
    assert_eq!(res["interrupted"], true);

    let events = drain_events(&mut rx, Duration::from_secs(10), |e| {
        e.event == "turn" && e.data["state"] == "ended"
    })
    .await;
    assert!(
        events.iter().any(|e| e.event == "turn"
            && e.data["state"] == "ended"
            && e.data["stop_reason"] == "cancelled"),
        "the interrupted turn ended cancelled"
    );

    let chat = ts.client.get("/api/sessions/acp-int/chat").await.unwrap();
    let blocks = chat["blocks"].as_array().unwrap();
    let turn_end = blocks
        .iter()
        .find(|b| b["kind"] == "turn_end")
        .expect("a turn_end block");
    assert_eq!(turn_end["payload"]["stop_reason"], "cancelled");
}

/// The `/chat` routes reject a terminal-backend session with 409.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_routes_reject_terminal_sessions() {
    let ts = TestServer::start().await;
    let ws = ts
        .client
        .post(
            "/api/sessions",
            json!({ "goal": "terminal", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = ws["id"].as_str().unwrap().to_string();

    assert!(
        ts.client
            .get(&format!("/api/sessions/{id}/chat"))
            .await
            .is_err(),
        "a terminal session has no chat journal"
    );
    assert!(
        ts.client
            .post(
                &format!("/api/sessions/{id}/prompt"),
                json!({ "text": "hi" })
            )
            .await
            .is_err(),
        "a terminal session has no /prompt"
    );

    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// REST create → turn-driven lifecycle → adopt / archive / preview
//
// Phase-4's protocol axis and turn-driven lifecycle over the *public* API: a
// custom agent whose ACP adapter is the scripted fake, created through
// `POST /api/sessions`, then driven and torn down exactly as the dashboard does.
// ---------------------------------------------------------------------------

/// Seed a custom agent whose ACP `launch` command is the scripted fake adapter,
/// so `POST /api/sessions` resolves `protocol='acp'` and brings it up over a relay.
async fn seed_acp_agent(ts: &TestServer, name: &str) {
    loom::custom_agents::set(
        &ts.state.db,
        &loom::custom_agents::CustomAgent {
            name: name.to_string(),
            label: "Fake ACP".to_string(),
            setup: String::new(),
            launch: agent_cmd(),
            resume: String::new(),
            reports_status: false,
            protocol: "acp".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        },
    )
    .await
    .unwrap();
}

/// REST-create a session with `goal` against `agent`, returning the `SessionView`.
async fn rest_create(ts: &TestServer, agent: &str, goal: &str) -> Value {
    ts.client
        .post(
            "/api/sessions",
            json!({ "goal": goal, "cwd": ts.cwd(), "agent": agent }),
        )
        .await
        .expect("acp session creates")
}

/// Poll `GET /api/sessions/{id}` until `pred` accepts the view, returning it.
async fn poll_view(
    ts: &TestServer,
    id: &str,
    timeout: Duration,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let view = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
        if pred(&view) {
            return view;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("session view never satisfied the predicate; last: {view}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A. REST create stamps `protocol='acp'`, seeds the goal as turn 0, and the
///    turn-driven lifecycle runs: turn end stamps the quiet `idle` mark, and a
///    `/send` dispatches turn 1 (clearing `idle`) while recording the nudge audit.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_create_drives_the_turn_lifecycle() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fakeacp").await;

    let created = rest_create(&ts, "fakeacp", "say:hello").await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["protocol"], "acp", "the session row is stamped acp");

    // The goal dispatched turn 0: the journal holds the goal as the first
    // `user_message`, the agent's reply, and a `turn_end` once it settles.
    let chat = poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();
    let user0 = blocks
        .iter()
        .find(|b| b["kind"] == "user_message" && b["turn"] == 0)
        .expect("a turn-0 user_message");
    assert!(
        user0["payload"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("say:hello"),
        "the goal seeded turn 0's user_message: {user0}"
    );
    assert_eq!(
        count_kind(blocks, "agent_message"),
        1,
        "the goal turn replied"
    );

    // Turn end ⇒ the quiet `idle` mark (the ACP task's `idle` lifecycle edge),
    // and the live session reads `running`.
    let view = poll_view(&ts, &id, Duration::from_secs(10), |v| {
        branch_tag_value(v, "idle") == "idle"
    })
    .await;
    assert_eq!(view["status"], "running", "the live session reads running");

    // A send during idle dispatches at once as turn 1 and clears `idle` (the
    // `working` edge). The `wait` keeps the turn live long enough to observe it.
    let sent = ts
        .client
        .post(
            &format!("/api/sessions/{id}/send"),
            json!({ "text": "wait:1500|say:again" }),
        )
        .await
        .unwrap();
    assert_eq!(
        sent["queued"], false,
        "an idle session dispatches the send at once"
    );
    assert_eq!(sent["turn"], 1, "the send opened turn 1");

    // The `working` edge cleared the `idle` mark...
    poll_view(&ts, &id, Duration::from_secs(5), |v| {
        branch_tag_value(v, "idle").is_empty()
    })
    .await;
    // ...and the send became turn 1's user_message.
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| {
            b["kind"] == "user_message"
                && b["turn"] == 1
                && b["payload"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("say:again")
        })
    })
    .await;

    // The send is also a `nudge` audit event — parity with the terminal path.
    let nudges = loom::events::since(&ts.state.db, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "nudge")
        .count();
    assert_eq!(nudges, 1, "the send recorded exactly one nudge audit event");
}

/// A provider handoff keeps loom's identity and canonical journal, records one
/// compact boundary instead of the synthetic bootstrap prompt, and continues at
/// the next turn under the replacement adapter.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_replaces_provider_and_continues_the_journal() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fake-a").await;
    seed_acp_agent(&ts, "fake-b").await;

    let created = rest_create(&ts, "fake-a", "say:before").await;
    let id = created["id"].as_str().unwrap().to_string();
    let branch_id = created["branch"]["id"].clone();
    let work_dir = created["work_dir"].clone();
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;

    let handed = ts
        .client
        .post(
            &format!("/api/sessions/{id}/handoff"),
            json!({ "agent": "fake-b" }),
        )
        .await
        .expect("handoff succeeds");
    assert_eq!(handed["id"], id, "loom session id stays stable");
    assert_eq!(handed["branch"]["id"], branch_id);
    assert_eq!(handed["work_dir"], work_dir);
    assert_eq!(handed["agent_kind"], "fake-b");
    assert!(handed["acp_session_id"].as_str().is_some());

    let chat = poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().filter(|b| b["kind"] == "turn_end").count() >= 2
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();
    let handoffs: Vec<&Value> = blocks.iter().filter(|b| b["kind"] == "handoff").collect();
    assert_eq!(handoffs.len(), 1, "one durable provider boundary");
    assert_eq!(handoffs[0]["turn"], 1);
    assert_eq!(handoffs[0]["seq"], 0);
    assert_eq!(handoffs[0]["payload"]["from"], "fake-a");
    assert_eq!(handoffs[0]["payload"]["to"], "fake-b");
    assert_eq!(
        count_kind(blocks, "user_message"),
        1,
        "the synthetic handoff bootstrap is not shown as a human message"
    );
    assert!(blocks
        .iter()
        .any(|b| { b["kind"] == "agent_message" && b["payload"]["text"] == "before" }));

    ts.client
        .post(
            &format!("/api/sessions/{id}/prompt"),
            json!({ "text": "say:after" }),
        )
        .await
        .expect("replacement accepts later work");
    let chat = poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "after")
    })
    .await;
    assert!(chat["blocks"].as_array().unwrap().iter().any(|b| {
        b["kind"] == "user_message" && b["turn"] == 2 && b["payload"]["text"] == "say:after"
    }));
}

/// Handoff is ordered with prompts on the task command channel: a turn that
/// starts first wins and the provider remains untouched.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_rejects_an_inflight_turn_without_stopping_it() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fake-a").await;
    seed_acp_agent(&ts, "fake-b").await;
    let created = rest_create(&ts, "fake-a", "say:ready").await;
    let id = created["id"].as_str().unwrap().to_string();
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;

    ts.client
        .post(
            &format!("/api/sessions/{id}/send"),
            json!({ "text": "wait:500|say:finished" }),
        )
        .await
        .unwrap();
    let err = ts
        .client
        .post(
            &format!("/api/sessions/{id}/handoff"),
            json!({ "agent": "fake-b" }),
        )
        .await
        .expect_err("live turn blocks handoff");
    assert!(
        err.to_string().contains("cannot hand off while a turn"),
        "{err}"
    );
    let view = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(view["agent_kind"], "fake-a", "old provider stays live");
    poll_chat(&ts, &id, Duration::from_secs(10), |blocks| {
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "finished")
    })
    .await;
}

/// Once the old provider is quiesced, a replacement handshake failure leaves a
/// coherent visible error and no leaked relay/task.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handoff_failure_cleans_up_the_replacement() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fake-a").await;
    loom::custom_agents::set(
        &ts.state.db,
        &loom::custom_agents::CustomAgent {
            name: "broken-acp".to_string(),
            label: "Broken ACP".to_string(),
            setup: String::new(),
            launch: "exit 7".to_string(),
            resume: String::new(),
            reports_status: false,
            protocol: "acp".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        },
    )
    .await
    .unwrap();
    let created = rest_create(&ts, "fake-a", "say:ready").await;
    let id = created["id"].as_str().unwrap().to_string();
    let relay = created["term_session"].as_str().unwrap().to_string();
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;

    let err = ts
        .client
        .post(
            &format!("/api/sessions/{id}/handoff"),
            json!({ "agent": "broken-acp" }),
        )
        .await
        .expect_err("broken replacement fails");
    assert!(err.to_string().contains("agent handoff failed"), "{err}");
    let view = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert_eq!(view["status"], "error");
    assert_eq!(view["agent_kind"], "broken-acp");
    assert_eq!(view["acp_session_id"], Value::Null);
    assert!(!ts.state.acp.is_live(&id), "replacement task is gone");
    assert!(
        !backend::has_session(&relay).await,
        "replacement relay is gone"
    );
}

/// B. Adopt after a full crash (task + relay gone): the monitor orphans the row,
///    `/adopt` respawns the relay and reopens via `session/load`, and the journal
///    continues without duplicating the replayed history.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_reopens_via_load_without_duplicates() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fakeacp").await;

    let created = rest_create(&ts, "fakeacp", "say:recovered").await;
    let id = created["id"].as_str().unwrap().to_string();
    let term_session = created["term_session"].as_str().unwrap().to_string();

    // Let the goal turn settle (journal: user_message + agent_message + turn_end).
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;
    let view = ts.client.get(&format!("/api/sessions/{id}")).await.unwrap();
    assert!(
        view["acp_session_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("fake-session-"),
        "the adapter session id is stored for a later load"
    );

    // Full crash: drop the loom-side task *and* kill the relay supervisor, so the
    // adapter is gone (the `session/load` respawn path, not a re-attach).
    assert!(ts.state.acp.stop(&id), "a task was running");
    backend::kill_session(&term_session).await.ok();

    // The monitor notices the dead terminal and marks the row orphaned.
    poll_view(&ts, &id, Duration::from_secs(15), |v| {
        v["status"] == "orphaned"
    })
    .await;

    // Adopt: respawn + `session/load`. The replayed history dedups against the
    // existing journal, so the counts are unchanged (no duplicate blocks).
    ts.client
        .post(&format!("/api/sessions/{id}/adopt"), json!({}))
        .await
        .expect("adopt succeeds");
    poll_view(&ts, &id, Duration::from_secs(10), |v| {
        v["status"] == "running"
    })
    .await;

    let chat = ts
        .client
        .get(&format!("/api/sessions/{id}/chat"))
        .await
        .unwrap();
    let blocks = chat["blocks"].as_array().unwrap();
    assert_eq!(
        count_kind(blocks, "user_message"),
        1,
        "one user_message (no load dup)"
    );
    assert_eq!(
        count_kind(blocks, "agent_message"),
        1,
        "one agent_message (no load dup)"
    );
    assert_eq!(
        count_kind(blocks, "turn_end"),
        1,
        "one turn_end (no load dup)"
    );

    // The journal *continues*: a post-adopt send opens a fresh turn 1 that appends
    // cleanly on top of the seeded cursor.
    ts.client
        .post(
            &format!("/api/sessions/{id}/send"),
            json!({ "text": "say:continued" }),
        )
        .await
        .expect("post-adopt send dispatches");
    let chat = poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        count_kind(blocks, "turn_end") >= 2
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();
    assert!(
        blocks.iter().any(|b| b["kind"] == "user_message"
            && b["turn"] == 1
            && b["payload"]["text"] == "say:continued"),
        "the post-adopt send became turn 1's user_message"
    );
    assert!(
        blocks
            .iter()
            .any(|b| b["kind"] == "agent_message" && b["payload"]["text"] == "continued"),
        "the continued turn ran to completion"
    );
}

/// C. `/conversation` serves the journal as an iris log live, and archiving writes
///    the same log to `chat.json` under the configured log dir.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_is_live_and_archive_captures_it() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fakeacp").await;

    // Pin the capture log dir to a temp dir so the archive never touches ~/.iris.
    let logs = tempfile::tempdir().unwrap();
    loom::config::apply(
        &ts.state.db,
        &[(
            "session.log_dir".to_string(),
            Some(logs.path().to_string_lossy().into_owned()),
        )],
    )
    .await
    .unwrap();

    let created = rest_create(&ts, "fakeacp", "say:archived").await;
    let id = created["id"].as_str().unwrap().to_string();
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;

    // `/conversation` maps the live journal to an iris log for the Conversation tab.
    let log = ts
        .client
        .get(&format!("/api/sessions/{id}/conversation"))
        .await
        .expect("live conversation serves the journal");
    assert_eq!(
        log["source"], "acp",
        "the journal maps to an acp-source log"
    );
    let messages = log["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["role"] == "user"
            && m["blocks"][0]["text"]
                .as_str()
                .unwrap_or("")
                .contains("say:archived")),
        "the goal shows as the user turn: {log}"
    );
    assert!(
        messages.iter().any(|m| m["role"] == "assistant"),
        "the agent reply shows as an assistant turn"
    );

    // Archiving captures the same log to `chat.json` (from the journal, not a
    // JSONL scrape) under the pinned log dir.
    ts.client
        .post(&format!("/api/sessions/{id}/archive"), json!({}))
        .await
        .expect("archive succeeds");

    // The capture lands under `<log_dir>/<branch-slug>/chat.json`.
    let branch_dir = std::fs::read_dir(logs.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("a captured branch dir");
    let chat_json =
        std::fs::read_to_string(branch_dir.join("chat.json")).expect("chat.json was written");
    assert!(chat_json.contains("\"source\": \"acp\""), "{chat_json}");
    assert!(
        chat_json.contains("say:archived"),
        "the goal survived the capture"
    );
}

/// D. `/preview` renders the last journal blocks as compact plain text — the CLI's
///    "what does this session look like right now" for an ACP session.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_renders_the_journal_tail_as_text() {
    let ts = TestServer::start().await;
    seed_acp_agent(&ts, "fakeacp").await;

    let created = rest_create(&ts, "fakeacp", "say:previewed").await;
    let id = created["id"].as_str().unwrap().to_string();
    poll_chat(&ts, &id, Duration::from_secs(15), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;

    let preview = ts
        .client
        .get(&format!("/api/sessions/{id}/preview"))
        .await
        .expect("preview renders the journal tail");
    let screen = preview["screen"].as_str().unwrap();
    assert!(
        screen.contains("[you]"),
        "the user line is rendered: {screen}"
    );
    assert!(
        screen.contains("say:previewed"),
        "the goal text is shown: {screen}"
    );
    assert!(
        screen.contains("[agent]"),
        "the agent line is rendered: {screen}"
    );
    assert!(
        screen.contains("· end_turn"),
        "the turn boundary is marked: {screen}"
    );
}

/// I. Phase 6, the builtin codex over codex-acp: a REST create with
///    `protocol: "acp"` resolves the `acp.codex_cmd` adapter (the fake here),
///    stamps the row, and drives a full goal turn through the journal.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_codex_launches_over_codex_acp() {
    // A builtin launch passes the GitHub-token gate; CI has no ambient token.
    let _token = EnvVarSet::set("GH_TOKEN", "test-token");
    let ts = TestServer::start().await;
    loom::config::apply(
        &ts.state.db,
        &[("acp.codex_cmd".to_string(), Some(agent_cmd()))],
    )
    .await
    .unwrap();

    let created = ts
        .client
        .post(
            "/api/sessions",
            json!({ "goal": "say:codex online", "cwd": ts.cwd(),
                    "agent": "codex", "protocol": "acp" }),
        )
        .await
        .expect("builtin codex creates over acp");
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["protocol"], "acp", "the session row is stamped acp");

    let chat = poll_chat(&ts, &id, Duration::from_secs(20), |blocks| {
        blocks.iter().any(|b| b["kind"] == "turn_end")
    })
    .await;
    let blocks = chat["blocks"].as_array().unwrap();
    let reply = blocks
        .iter()
        .find(|b| b["kind"] == "agent_message")
        .expect("the goal turn replied");
    assert_eq!(reply["payload"]["text"], "codex online");
}

/// J. Phase 6, the codex launch mapping: `build_acp_launch` resolves the
///    codex-acp adapter and maps model/effort/mode onto its env contract
///    (`CODEX_CONFIG`, `INITIAL_AGENT_MODE`, `DEFAULT_AUTH_REQUEST`) instead of
///    `_meta` + `session/set_mode`, with operator env winning over the defaults
///    and a primer-only launch seeding the primer positionally.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_acp_launch_maps_the_adapter_contract() {
    let ts = TestServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let goal = dir.path().join("goal.txt");
    let primer = dir.path().join("primer.txt");
    tokio::fs::write(&goal, "ship it").await.unwrap();
    tokio::fs::write(&primer, "orient first").await.unwrap();

    let env_of = |launch: &AcpLaunch, key: &str| {
        launch
            .env
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>()
    };
    let addr = ts.addr.to_string();
    let spec = |goal_file, extra_env| loom::agent::AcpLaunchSpec {
        branch_id: "b-codex",
        runtime: "codex",
        work_dir: dir.path(),
        server_addr: &addr,
        model: "gpt-5.3-codex",
        effort: "high",
        goal_file,
        primer_file: Some(primer.as_path()),
        extra_env,
        mode: "bypassPermissions",
        custom: None,
    };

    let launch = loom::agent::build_acp_launch(
        &ts.state.db,
        &spec(Some(goal.as_path()), &[]),
        loom::agent::AcpOpen::Fresh,
    )
    .await
    .unwrap();
    assert_eq!(
        launch.adapter_cmd,
        "command -v codex-acp >/dev/null 2>&1 && exec codex-acp; \
         exec npx --yes @agentclientprotocol/codex-acp",
        "the npm default (installed bin, else npx) resolves when neither env nor config names one"
    );
    assert_eq!(
        env_of(&launch, "DEFAULT_AUTH_REQUEST"),
        vec![r#"{"methodId":"api-key"}"#.to_string()]
    );
    assert_eq!(
        env_of(&launch, "INITIAL_AGENT_MODE"),
        vec!["agent-full-access"]
    );
    let cfg: Value = serde_json::from_str(&env_of(&launch, "CODEX_CONFIG")[0]).unwrap();
    assert_eq!(cfg["model"], "gpt-5.3-codex");
    assert_eq!(cfg["model_reasoning_effort"], "high");
    assert!(
        launch.mode.is_none(),
        "the mode boots via INITIAL_AGENT_MODE, not a claude-id set_mode"
    );
    assert_eq!(launch.goal.as_deref(), Some("ship it"));
    match &launch.new_or_load {
        NewOrLoad::New { meta, .. } => assert!(meta.is_none(), "codex takes no _meta"),
        NewOrLoad::Load { .. } => panic!("a fresh launch opens session/new"),
    }

    // An operator-provided CODEX_CONFIG wins; a goalless launch seeds the primer.
    let operator = [(
        "CODEX_CONFIG".to_string(),
        r#"{"model":"mine"}"#.to_string(),
    )];
    let launch = loom::agent::build_acp_launch(
        &ts.state.db,
        &spec(None, &operator),
        loom::agent::AcpOpen::Fresh,
    )
    .await
    .unwrap();
    assert_eq!(env_of(&launch, "CODEX_CONFIG"), vec![r#"{"model":"mine"}"#]);
    assert_eq!(launch.goal.as_deref(), Some("orient first"));
}

/// K. Phase 7, adopt-after-the-flip: an orphaned *terminal* session whose
///    builtin runtime now declares acp is adopted into acp — the relay respawns
///    the adapter (the fake here, via `acp.claude_cmd`), and the handshake
///    stamps the row `protocol='acp'` with the adapter's session id. With no
///    on-disk claude conversation recorded for the worktree, the reopen is a
///    fresh `session/new`.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_converts_a_terminal_builtin_session_to_acp() {
    let ts = TestServer::start().await;
    loom::config::apply(
        &ts.state.db,
        &[("acp.claude_cmd".to_string(), Some(agent_cmd()))],
    )
    .await
    .unwrap();

    let branch = loom::branch::upsert(&ts.state.db, &ts.cwd(), "weaver/acp-convert", "main")
        .await
        .unwrap();
    session_mod::insert(
        &ts.state.db,
        &NewSession {
            id: "acp-convert".to_string(),
            branch_id: branch.id,
            work_dir: ts.cwd(),
            term_session: "weaver-acp-convert".to_string(),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "orphaned".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: None,
            protocol: "terminal".to_string(),
        },
    )
    .await
    .unwrap();

    ts.client
        .post("/api/sessions/acp-convert/adopt", json!({}))
        .await
        .expect("the terminal session adopts");

    let view = poll_view(&ts, "acp-convert", Duration::from_secs(15), |v| {
        v["protocol"] == "acp"
    })
    .await;
    assert!(
        view["acp_session_id"]
            .as_str()
            .unwrap()
            .starts_with("fake-session-"),
        "the handshake stamped the adapter's session id: {view}"
    );
    assert_eq!(view["status"], "running");
}
