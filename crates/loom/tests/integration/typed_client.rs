//! The typed `weaver_api::Client` methods, round-tripped against a real server.
//!
//! These exercise the typed surface the Python binding wraps — `create_session`,
//! `list_sessions`, `get_session`, and `mark` (triage) — deserializing real
//! `SessionView`s rather than poking at raw JSON. They cover the DTO contract
//! end-to-end: the server serializes the moved `weaver-api` structs and the
//! client deserializes the same definitions.

use serial_test::serial;

use weaver_api::CreateReq;

use crate::fixtures::TestServer;

/// The value of a typed `BranchView`'s tag by key, or `None` when absent.
fn tag_value<'a>(branch: &'a weaver_api::BranchView, key: &str) -> Option<&'a str> {
    branch
        .tags
        .iter()
        .find(|t| t.key == key)
        .map(|t| t.value.as_str())
}

/// A typed create → list → get → mark cycle. The view fields deserialize from
/// the server's JSON, and the triage mark round-trips onto the session's branch
/// `tags` without disturbing the agent's own (absent ⇒ `ok`) attention.
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
    assert!(
        tag_value(&got.branch, "attention").is_none(),
        "agent attention starts calm (no tag)"
    );
    assert!(
        tag_value(&got.branch, "triage").is_none(),
        "unmarked at first"
    );

    // Typed mark (triage): stamps the watch axis as the `triage` tag, the
    // agent's own `attention` tag untouched.
    let marked = client
        .mark(&id, "attention", "looks stuck", Some("typed-test"))
        .await
        .unwrap();
    let triage = marked
        .branch
        .tags
        .iter()
        .find(|t| t.key == "triage")
        .expect("the mark wrote a triage tag");
    assert_eq!(triage.value, "attention");
    assert_eq!(triage.note, "looks stuck");
    assert_eq!(triage.set_by, "typed-test");
    assert!(!triage.set_at.is_empty(), "mark stamps a timestamp");
    assert!(
        tag_value(&marked.branch, "attention").is_none(),
        "the mark never touches the agent's own attention"
    );

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}
