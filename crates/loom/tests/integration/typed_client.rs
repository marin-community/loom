//! The typed `weaver_api::Client` methods, round-tripped against a real server.
//!
//! These exercise the typed surface the Python binding wraps — `create_session`,
//! `list_sessions`, `get_session`, and `mark` (triage) — deserializing real
//! `SessionView`s rather than poking at raw JSON. They cover the DTO contract
//! end-to-end: the server serializes the moved `weaver-api` structs and the
//! client deserializes the same definitions.

use serial_test::serial;

use serde_json::{Map, Value};
use weaver_api::{CreateReq, SettingKind};

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
    assert!(
        created.tracking_issue.is_none(),
        "ordinary sessions coordinate through channels, not automatic issues"
    );
    let id = created.id.clone();

    // Session creation atomically creates its same-id default channel and
    // immutable opening goal message.
    let channels = client.list_channels(false).await.unwrap();
    let channel = channels
        .iter()
        .find(|channel| channel.id == id)
        .expect("created session has a default channel");
    assert_eq!(channel.session_id.as_deref(), Some(id.as_str()));
    assert_eq!(channel.topic, "typed client round-trip");
    let opening = client.channel_messages(&id, 0).await.unwrap();
    assert_eq!(opening.len(), 1);
    assert_eq!(opening[0].kind, "goal");
    assert_eq!(opening[0].body, "typed client round-trip");

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

    // Agent status is also a typed channel item while the compatibility tag
    // continues to drive the existing dashboard and mirrors.
    client
        .set_branch_status(&created.branch.id, "attention", "review the boundary")
        .await
        .unwrap();
    let status = client.channel_messages(&id, 1).await.unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].kind, "status");
    assert_eq!(status[0].urgency, "attention");
    assert_eq!(status[0].body, "review the boundary");

    client.delete(&format!("/api/sessions/{id}")).await.unwrap();
}

/// Both settings methods share one rich typed envelope. Registry metadata must
/// survive a PATCH reply just as it does the initial GET.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_settings_get_and_patch_share_the_rich_envelope() {
    let ts = TestServer::start_api_only().await;
    let client = &ts.client;

    let initial = client.list_settings().await.unwrap();
    let initial_setting = initial
        .settings
        .iter()
        .find(|setting| setting.key == "server.auto_adopt")
        .expect("server.auto_adopt is registered");
    assert_eq!(initial_setting.kind, SettingKind::Bool);
    assert!(!initial_setting.label.is_empty());
    assert!(!initial_setting.description.is_empty());
    assert_eq!(initial_setting.default, "false");
    assert!(!initial_setting.group.is_empty());
    assert!(initial_setting.options.is_empty());

    let mut changes = Map::new();
    changes.insert("server.auto_adopt".to_string(), Value::Bool(true));
    let updated = client.patch_settings(changes).await.unwrap();
    let updated_setting = updated
        .settings
        .iter()
        .find(|setting| setting.key == "server.auto_adopt")
        .expect("PATCH returns the full registry");
    assert_eq!(updated_setting.kind, SettingKind::Bool);
    assert_eq!(updated_setting.label, initial_setting.label);
    assert_eq!(updated_setting.description, initial_setting.description);
    assert_eq!(updated_setting.default, initial_setting.default);
    assert_eq!(updated_setting.group, initial_setting.group);
    assert_eq!(updated_setting.options, initial_setting.options);
    assert_eq!(updated_setting.value, "true");
    assert!(!updated_setting.is_default);
}
