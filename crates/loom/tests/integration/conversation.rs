//! `sessions.conversation` — the normalized iris log behind the dashboard's
//! Conversation tab.

use serde_json::json;
use serial_test::serial;

use crate::fixtures::{plant_claude_transcript, HomeGuard, TestServer};

/// With a transcript present, the endpoint returns the parsed iris log: source,
/// model, and the user/assistant turns the viewer renders.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_endpoint_returns_the_iris_log() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());

    let sess = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "chat me", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    let work_dir = sess["work_dir"].as_str().unwrap().to_string();

    // Before any transcript exists, the endpoint 404s (no conversation yet).
    assert!(
        client
            .post("/api/sessions/conversation", json!({ "session": id }))
            .await
            .is_err(),
        "no transcript yet → 404"
    );

    plant_claude_transcript(home.path(), &work_dir, "do the work", "Working on it.");

    let log = client
        .post("/api/sessions/conversation", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(log["source"], "claude");
    assert_eq!(log["model"], "claude-opus-4-8");
    let messages = log["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["blocks"][0]["kind"], "text");
    assert_eq!(messages[0]["blocks"][0]["text"], "do the work");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["blocks"][0]["text"], "Working on it.");

    // The normalized history resource reads the same live terminal transcript,
    // but gives agents a bounded, cursor-paged record contract.
    let latest = client
        .post(
            "/api/sessions/history/list",
            json!({ "session": id, "limit": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(latest["source"], "claude");
    assert_eq!(latest["records"].as_array().unwrap().len(), 1);
    assert_eq!(latest["records"][0]["kind"], "message");
    assert_eq!(latest["records"][0]["role"], "assistant");
    assert_eq!(latest["records"][0]["content"], "Working on it.");
    let cursor = latest["older_cursor"].as_str().unwrap();

    let older = client
        .post(
            "/api/sessions/history/list",
            json!({ "session": id, "limit": 1, "before": cursor }),
        )
        .await
        .unwrap();
    assert_eq!(older["records"][0]["role"], "user");
    assert_eq!(older["records"][0]["content"], "do the work");
    assert!(older.get("older_cursor").is_none());

    let search = client
        .post(
            "/api/sessions/history/search",
            json!({ "session": id, "q": "WORKING", "kinds": ["message"] }),
        )
        .await
        .unwrap();
    assert_eq!(search["records"].as_array().unwrap().len(), 1);
    assert_eq!(search["records"][0]["role"], "assistant");

    // A cue read is source-linked but never invokes a model. With no metadata
    // profile configured it exposes an API-ready unavailable boundary and the
    // exact conversation/artifact cursor that a later ensure would summarize.
    let unavailable = client
        .post("/api/sessions/resumption_cue/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(unavailable["status"], "unavailable");
    let source_cursor = unavailable["source_cursor"].as_str().unwrap();
    let evidence = unavailable["evidence"].as_array().unwrap();
    assert!(evidence.iter().any(|item| item["kind"] == "conversation"));
    assert!(evidence.iter().any(|item| item["kind"] == "artifact"
        && item["cursor"]
            .as_str()
            .is_some_and(|cursor| cursor.ends_with(":1"))));

    // Seed the persisted result a completed one-shot would commit. GET and an
    // on-return ensure both reuse it while the source cursor is unchanged.
    sqlx::query(
        "UPDATE session_metadata_assistance
         SET cue_source_cursor = ?, cue_text = ?, cue_generated_at = ?,
             cue_evidence = ?, updated_at = ?
         WHERE session_id = ?",
    )
    .bind(source_cursor)
    .bind("Continue from the verified transcript.")
    .bind("2026-07-26T00:00:00Z")
    .bind(serde_json::to_string(evidence).unwrap())
    .bind("2026-07-26T00:00:00Z")
    .bind(&id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    let cached = client
        .post("/api/sessions/resumption_cue/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(cached["status"], "generated");
    assert_eq!(cached["text"], "Continue from the verified transcript.");
    let ensured = client
        .post(
            "/api/sessions/resumption_cue/ensure",
            json!({ "session": id, "force": false }),
        )
        .await
        .unwrap();
    assert_eq!(ensured, cached);

    // Advancing an artifact revision invalidates the cached cue even though the
    // terminal transcript itself is unchanged.
    let branch_id = sess["branch"]["id"].as_str().unwrap();
    weaver_core::branch::set_goal(&ts.state.db, branch_id, "chat me, revised", "user")
        .await
        .unwrap();
    let advanced = client
        .post("/api/sessions/resumption_cue/get", json!({ "session": id }))
        .await
        .unwrap();
    assert_eq!(advanced["status"], "unavailable");
    assert_ne!(advanced["source_cursor"], cached["source_cursor"]);
    assert!(advanced["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "artifact"
            && item["cursor"]
                .as_str()
                .is_some_and(|cursor| cursor.ends_with(":2"))));
}

/// The ACP endpoint opens at a bounded tail and pages backward with an exclusive
/// cursor. A long DB journal must not become one unbounded response.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_endpoint_pages_long_journals_from_the_tail() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "long chat", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE sessions SET protocol = 'acp' WHERE id = ?")
        .bind(&id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    for seq in 0..205 {
        loom::chat::insert(
            &ts.state.db,
            &id,
            0,
            seq,
            loom::chat::kind::AGENT_MESSAGE,
            &json!({ "text": seq.to_string() }),
        )
        .await
        .unwrap();
    }

    let latest = client
        .post("/api/sessions/chat", json!({ "session": id }))
        .await
        .unwrap();
    let blocks = latest["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 200);
    assert_eq!(blocks.first().unwrap()["seq"], 5);
    assert_eq!(blocks.last().unwrap()["seq"], 204);
    assert_eq!(latest["older_cursor"], json!({ "turn": 0, "seq": 5 }));

    let older = client
        .post(
            "/api/sessions/chat",
            json!({ "session": id, "before_turn": 0, "before_seq": 5 }),
        )
        .await
        .unwrap();
    let blocks = older["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 5);
    assert_eq!(blocks.first().unwrap()["seq"], 0);
    assert_eq!(blocks.last().unwrap()["seq"], 4);
    assert!(older["older_cursor"].is_null());
}

/// ACP tool records expose only fields the protocol supplied. In particular,
/// content and locations are not mislabeled as invocation arguments.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_history_search_is_literal_filtered_and_honest_about_tool_input() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    let sess = client
        .post(
            "/api/sessions/launch",
            json!({ "goal": "search tools", "cwd": ts.cwd(), "agent": "shell" }),
        )
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE sessions SET protocol = 'acp' WHERE id = ?")
        .bind(&id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    loom::chat::insert(
        &ts.state.db,
        &id,
        0,
        0,
        loom::chat::kind::USER_MESSAGE,
        &json!({ "text": "please run the focused check" }),
    )
    .await
    .unwrap();
    loom::chat::insert(
        &ts.state.db,
        &id,
        0,
        1,
        loom::chat::kind::TOOL_CALL,
        &json!({
            "tool_call_id": "call-1",
            "title": "Run focused check",
            "tool_kind": "execute",
            "status": "completed",
            "content": [{ "type": "text", "text": "all green" }],
            "locations": [{ "path": "/repo/src/lib.rs", "line": 9 }]
        }),
    )
    .await
    .unwrap();

    let page = client
        .post(
            "/api/sessions/history/search",
            json!({ "session": id, "q": "GREEN", "kinds": ["tool_call"] }),
        )
        .await
        .unwrap();
    assert_eq!(page["source"], "acp");
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    let tool = &page["records"][0];
    assert_eq!(tool["kind"], "tool_call");
    assert_eq!(tool["tool_name"], "Run focused check");
    assert_eq!(tool["tool_status"], "completed");
    assert_eq!(tool["content"], "all green");
    assert_eq!(tool["locations"][0]["path"], "/repo/src/lib.rs");
    assert!(
        tool.get("tool_input").is_none(),
        "ACP did not supply invocation arguments"
    );

    assert!(
        client
            .post(
                "/api/sessions/history/list",
                json!({ "session": id, "kinds": ["made_up"] }),
            )
            .await
            .is_err(),
        "unknown filters fail closed"
    );
    assert!(
        client
            .post(
                "/api/sessions/history/search",
                json!({ "session": id, "q": "" }),
            )
            .await
            .is_err(),
        "empty searches are rejected"
    );
}
