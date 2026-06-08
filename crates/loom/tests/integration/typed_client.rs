//! The typed `weaver_api::Client` methods, round-tripped against a real server.
//!
//! These exercise the typed surface the Python binding wraps — `create_session`,
//! `list_sessions`, `get_session`, and `mark` (triage) — deserializing real
//! `SessionView`s rather than poking at raw JSON. They cover the DTO contract
//! end-to-end: the server serializes the moved `weaver-api` structs and the
//! client deserializes the same definitions.

use serial_test::serial;

use weaver_api::{CreateReq, TriageReq};

use crate::fixtures::TestServer;

/// A typed create → list → get → mark cycle. The view fields deserialize from
/// the server's JSON, and the triage mark round-trips onto the session's branch
/// without disturbing the agent's own (default `ok`) attention.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_create_list_get_and_mark() {
    let ts = TestServer::start().await;
    let client = &ts.client;

    // Typed create: build a CreateReq, get a SessionView back.
    let created = client
        .create_session(&CreateReq {
            cwd: ts.cwd(),
            goal: Some("typed client round-trip".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(created.branch.name, "typed-client-round-trip");
    assert_eq!(created.branch.title, "typed client round-trip");
    // The create path is the one that fills the tracking-issue handle.
    assert!(
        created.tracking_issue.is_some(),
        "create returns a tracking issue id"
    );
    let id = created.id.clone();

    // Typed list: the new session is the only one.
    let sessions = client.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);

    // Typed get by id.
    let got = client.get_session(&id).await.unwrap();
    assert_eq!(got.id, id);
    assert_eq!(got.branch.attention, "ok", "agent attention starts ok");
    assert_eq!(got.branch.triage_level, "", "unmarked at first");

    // Typed mark (triage): stamps the overlooker axis, agent attention untouched.
    let marked = client
        .mark(
            &id,
            &TriageReq {
                level: "attention".to_string(),
                note: "looks stuck".to_string(),
                by: Some("typed-test".to_string()),
            },
        )
        .await
        .unwrap();
    assert_eq!(marked.branch.triage_level, "attention");
    assert_eq!(marked.branch.triage_note, "looks stuck");
    assert_eq!(marked.branch.triage_by, "typed-test");
    assert!(marked.branch.triage_at.is_some(), "mark stamps a timestamp");
    assert_eq!(
        marked.branch.attention, "ok",
        "the mark never touches the agent's own attention"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}
