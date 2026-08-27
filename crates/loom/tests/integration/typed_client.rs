//! The typed `weaver_api::Client` methods, round-tripped against a real server.
//!
//! These exercise the typed client methods the Python binding wraps —
//! `create_session`, `list_sessions`, `get_session`, and `mark` (triage) —
//! deserializing real `SessionView`s rather than raw JSON. They cover the DTO
//! contract end-to-end: the server serializes the moved `weaver-api` structs
//! and the client deserializes the same definitions.

use serial_test::serial;

use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use weaver_api::{SettingKind, SettingSource};

use weaver_api::operations::permissions as permission_ops;

use crate::fixtures::TestServer;
use weaver_api::operations::{branches, channels, sessions, settings};

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

    let created = client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: (Some("typed client round-trip".to_string())).clone(),
            cwd: (ts.cwd()).clone(),
            agent: (Some("shell".to_string())).clone(),
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
    let channels = client
        .invoke::<channels::list::Op>(&channels::list::Input {
            archived: false,
            branch: String::new(),
        })
        .await
        .unwrap();
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

    // The session credential implies its own session and branch, so `context`
    // and the bindings lookup below can omit those ids.
    let session_token =
        loom::auth::create_session_token(&ts.state.db, Some("rjpower"), &id, &created.branch.id)
            .await
            .unwrap();
    let session_client =
        weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(session_token));
    let context = session_client
        .invoke::<sessions::context::Op>(&sessions::context::Input {
            session: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(context.session_id, id);
    assert_eq!(context.branch_id, created.branch.id);
    assert_eq!(context.channel_id, id);
    assert_eq!(context.links.channel, "/api/channels/get");
    let bindings = session_client
        .invoke::<channels::bindings::list::Op>(&channels::bindings::list::Input {
            channel: id.to_string(),
            branch: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].id, format!("session:{id}"));
    assert_eq!(bindings[0].label, "this session");

    // Retrying an append with the same key returns the original durable item.
    let result_request = channels::messages::create::Input {
        channel: id.to_string(),
        kind: "result".to_string(),
        urgency: "normal".to_string(),
        body: "done once".to_string(),
        payload: serde_json::json!({}),
        reply_to: None,
        idempotency_key: Some("typed-result-once".to_string()),
        branch: String::new(),
    };
    let first = session_client
        .invoke::<channels::messages::create::Op>(&result_request)
        .await
        .unwrap();
    let retry = session_client
        .invoke::<channels::messages::create::Op>(&result_request)
        .await
        .unwrap();
    assert_eq!(retry.id, first.id);
    assert_eq!(retry.seq, first.seq);
    assert_eq!(
        session_client
            .invoke::<channels::messages::list::Op>(&channels::messages::list::Input {
                channel: id.to_string(),
                after: 0,
                limit: 1_i64,
                kinds: Vec::new(),
                peek: false,
                branch: String::new(),
            })
            .await
            .unwrap()
            .len(),
        1
    );

    // The new session is the only one.
    let sessions = client.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);

    let got = client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: id.to_string(),
        })
        .await
        .unwrap();
    assert_eq!(got.id, id);
    assert!(
        tag_value(&got.branch, "attention").is_none(),
        "agent attention starts calm (no tag)"
    );
    assert!(
        tag_value(&got.branch, "triage").is_none(),
        "unmarked at first"
    );

    // Marking sets the `triage` tag; the agent's own `attention` tag stays
    // untouched.
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
        .invoke::<branches::status::set::Op>(&branches::status::set::Input {
            level: "attention".to_string(),
            message: Some("review the boundary".to_string()),
            branch: created.branch.id.to_string(),
        })
        .await
        .unwrap();
    let status = client.channel_messages(&id, first.seq).await.unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].kind, "status");
    assert_eq!(status[0].urgency, "attention");
    assert_eq!(status[0].body, "review the boundary");

    client
        .post("/api/sessions/delete", json!({ "session": id }))
        .await
        .unwrap();
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operation_discovery_and_permission_request_round_trip() {
    let ts = TestServer::start().await;
    let created = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: (Some("request repository access".to_string())).clone(),
            cwd: (ts.cwd()).clone(),
            agent: (Some("shell".to_string())).clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        &created.id,
        &created.branch.id,
    )
    .await
    .unwrap();
    let session = weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token));

    let meta = session.api_meta().await.unwrap();
    assert_eq!(meta.product, "loom");
    let operations = session.operations().await.unwrap();
    let request_operation = operations
        .iter()
        .find(|operation| operation.id == "permissions.requests.create")
        .expect("bound permission request operation is discoverable");
    assert_eq!(request_operation.method, "POST");
    assert_eq!(request_operation.path, "/api/permissions/requests/create");

    let request = session
        .invoke::<permission_ops::requests::create::Op>(&permission_ops::requests::create::Input {
            repository: "acme/widgets".to_string(),
            reason: "update the shared client".to_string(),
            mode: "write".to_string(),
            session: created.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(request.state, "pending");
    assert_eq!(
        session
            .invoke::<permission_ops::effective::get::Op>(&permission_ops::effective::get::Input {
                session: created.id.clone(),
            })
            .await
            .unwrap()
            .pending_requests[0]
            .id,
        request.id
    );
    let branch = session
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: created.id.to_string(),
        })
        .await
        .unwrap()
        .branch;
    assert_eq!(tag_value(&branch, "attention"), Some("attention"));

    let error = session
        .invoke::<permission_ops::requests::deny::Op>(&permission_ops::requests::deny::Input {
            request: request.id.clone(),
            reason: String::new(),
        })
        .await
        .unwrap_err();
    // `permissions.requests.deny` is declared `actor = User`, so the registry
    // itself refuses a session credential: this operation does not accept a
    // session grant.
    assert!(error.to_string().contains("human operator"));

    let denied = ts
        .client
        .invoke::<permission_ops::requests::deny::Op>(&permission_ops::requests::deny::Input {
            request: request.id.clone(),
            reason: "not needed".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(denied.state, "denied");
    assert_eq!(denied.decided_by.as_deref(), Some("rjpower"));
}

/// A GitHub App installation token covers one owner, so a session holding
/// access under two owners has no single token. Each repository is brokered on
/// its own, so granting the second owner mints only that repository, not the
/// union of both.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_credentials_are_brokered_per_repository_across_owners() {
    let ts = TestServer::start_with_app().await;
    let created = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            cwd: ts.cwd(),
            goal: Some("work across two owners".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET policy_github_repositories = ? WHERE id = ?")
        .bind(r#"["marin-community/marin"]"#)
        .bind(&created.id)
        .execute(&ts.state.db)
        .await
        .unwrap();

    // A human grants the second owner.
    ts.client
        .invoke::<permission_ops::github::grant::Op>(&permission_ops::github::grant::Input {
            session: created.id.clone(),
            repository: "Open-Athena/mumwelt".to_string(),
        })
        .await
        .unwrap();

    let token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        &created.id,
        &created.branch.id,
    )
    .await
    .unwrap();
    let session = weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token));

    assert_eq!(
        session
            .invoke::<permission_ops::effective::get::Op>(&permission_ops::effective::get::Input {
                session: created.id.clone(),
            })
            .await
            .unwrap()
            .github_repositories,
        ["Open-Athena/mumwelt", "marin-community/marin"]
    );

    for repository in ["marin-community/marin", "Open-Athena/mumwelt"] {
        let credential = session
            .invoke::<permission_ops::github::token::Op>(&permission_ops::github::token::Input {
                session: created.id.clone(),
                repository: Some(repository.to_string()),
            })
            .await
            .unwrap();
        assert!(!credential.token.is_empty(), "no token for {repository}");
    }

    // Omitting `repository` fails once the session's repositories span more
    // than one owner: there is no single token to return.
    let error = session
        .invoke::<permission_ops::github::token::Op>(&permission_ops::github::token::Input {
            session: created.id.clone(),
            repository: None,
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("one owner"), "unexpected error: {error}");

    // A repository the session has no grant for is refused before any minting.
    let denied = session
        .invoke::<permission_ops::github::token::Op>(&permission_ops::github::token::Input {
            session: created.id.clone(),
            repository: Some("marin-community/vllm".to_string()),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(
        denied.contains("no GitHub access to marin-community/vllm"),
        "unexpected error: {denied}"
    );
}

/// An `owner/*` launch-policy entry is a standing decision: a request for that
/// owner is applied immediately, with the same durable record a human decision
/// leaves, and without parking the session on someone who may not be reachable.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_pattern_applies_repository_access_without_a_human() {
    let ts = TestServer::start_with_app().await;
    let created = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            cwd: ts.cwd(),
            goal: Some("push to a sibling repository".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    // Stamp the pattern the way a profile allowlist would at launch.
    sqlx::query("UPDATE sessions SET policy_github_repositories = ? WHERE id = ?")
        .bind(r#"["marin-community/*"]"#)
        .bind(&created.id)
        .execute(&ts.state.db)
        .await
        .unwrap();

    let token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        &created.id,
        &created.branch.id,
    )
    .await
    .unwrap();
    let session = weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token));

    let granted = session
        .invoke::<permission_ops::requests::create::Op>(&permission_ops::requests::create::Input {
            session: created.id.clone(),
            repository: "marin-community/vllm".to_string(),
            mode: "write".to_string(),
            reason: "open the PR".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(granted.state, "approved");
    assert_eq!(
        granted.decided_by.as_deref(),
        Some("policy:marin-community/*")
    );

    let effective = session
        .invoke::<permission_ops::effective::get::Op>(&permission_ops::effective::get::Input {
            session: created.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(effective.github_repositories, ["marin-community/vllm"]);
    assert_eq!(effective.github_repository_patterns, ["marin-community/*"]);
    assert!(effective.pending_requests.is_empty());
    // A policy grant is not an interruption.
    let branch = session
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: created.id.clone(),
        })
        .await
        .unwrap()
        .branch;
    assert_ne!(tag_value(&branch, "attention"), Some("attention"));

    // An owner the policy says nothing about still needs a person.
    let pending = session
        .invoke::<permission_ops::requests::create::Op>(&permission_ops::requests::create::Input {
            session: created.id.clone(),
            repository: "acme/widgets".to_string(),
            mode: "write".to_string(),
            reason: "unrelated".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(pending.state, "pending");
    let branch = session
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: created.id.clone(),
        })
        .await
        .unwrap()
        .branch;
    assert_eq!(tag_value(&branch, "attention"), Some("attention"));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_result_delivers_once_to_the_bound_slack_origin() {
    let delivered = Arc::new(Mutex::new(Vec::<Value>::new()));
    let slack_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slack_addr = slack_listener.local_addr().unwrap();
    let slack_app = axum::Router::new()
        .route(
            "/chat.postMessage",
            axum::routing::post(
                |axum::extract::State(delivered): axum::extract::State<Arc<Mutex<Vec<Value>>>>,
                 axum::Json(body): axum::Json<Value>| async move {
                    delivered.lock().unwrap().push(body);
                    axum::Json(json!({ "ok": true, "ts": "1786.1234" }))
                },
            ),
        )
        .with_state(delivered.clone());
    let slack_task = tokio::spawn(async move {
        axum::serve(slack_listener, slack_app).await.unwrap();
    });
    std::env::set_var("LOOM_SLACK_API_BASE", format!("http://{slack_addr}"));

    let ts = TestServer::start().await;
    let created = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: (Some("deliver one canonical result".to_string())).clone(),
            cwd: (ts.cwd()).clone(),
            agent: (Some("shell".to_string())).clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    weaver_core::config::apply(
        &ts.state.db,
        &[("slack.bot_token".to_string(), Some("xoxb-test".to_string()))],
    )
    .await
    .unwrap();
    weaver_core::tags::set(
        &ts.state.db,
        &created.branch.id,
        loom::slack::WIRED_TAG,
        "T123/C123/1700000000.000001",
        "",
        "test",
    )
    .await
    .unwrap();
    let token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        &created.id,
        &created.branch.id,
    )
    .await
    .unwrap();
    let client = weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token));
    let request = channels::messages::create::Input {
        channel: created.id.to_string(),
        kind: "result".to_string(),
        urgency: "normal".to_string(),
        body: "the canonical answer".to_string(),
        payload: json!({}),
        reply_to: None,
        idempotency_key: Some("answer-once".to_string()),
        branch: String::new(),
    };
    let message = client
        .invoke::<channels::messages::create::Op>(&request)
        .await
        .unwrap();
    assert_eq!(message.deliveries.len(), 1);
    assert_eq!(message.deliveries[0].binding_id, "slack:origin");
    assert_eq!(message.deliveries[0].state, "delivered");
    assert_eq!(
        message.deliveries[0].external_id.as_deref(),
        Some("1786.1234")
    );

    // The legacy facade retries the same canonical item instead of posting a
    // second Slack reply after an ambiguous client-side failure.
    let retry = client
        .post(
            "/api/branches/slack/reply",
            json!({
                "branch": created.branch.id,
                "text": "the canonical answer",
                "idempotency_key": "answer-once"
            }),
        )
        .await
        .unwrap();
    assert_eq!(retry["message_id"], message.id);
    assert_eq!(delivered.lock().unwrap().len(), 1);
    let posted = delivered.lock().unwrap()[0].clone();
    assert_eq!(posted["channel"], "C123");
    assert_eq!(posted["thread_ts"], "1700000000.000001");
    assert_eq!(posted["text"], "the canonical answer");

    ts.client
        .post("/api/sessions/delete", json!({ "session": created.id }))
        .await
        .unwrap();
    weaver_core::config::apply(&ts.state.db, &[("slack.bot_token".to_string(), None)])
        .await
        .unwrap();
    std::env::remove_var("LOOM_SLACK_API_BASE");
    slack_task.abort();
}

/// Both settings methods share one rich typed envelope. Registry metadata must
/// survive a PATCH reply just as it does the initial GET.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_settings_get_and_patch_share_the_rich_envelope() {
    let ts = TestServer::start_api_only().await;
    let client = &ts.client;

    let initial = client
        .invoke::<settings::get::Op>(&settings::get::Input {})
        .await
        .unwrap();
    let initial_setting = initial
        .settings
        .iter()
        .find(|setting| setting.key == "server.auto_adopt")
        .expect("server.auto_adopt is registered");
    assert_eq!(initial_setting.kind, SettingKind::Bool);
    assert!(!initial_setting.label.is_empty());
    assert!(!initial_setting.description.is_empty());
    assert_eq!(initial_setting.default, "false");
    assert_eq!(initial_setting.source, SettingSource::Default);
    assert_eq!(initial_setting.deployment_value, None);
    assert!(!initial_setting.group.is_empty());
    assert!(initial_setting.options.is_empty());

    let mut changes = Map::new();
    changes.insert("server.auto_adopt".to_string(), Value::Bool(true));
    let updated = client
        .invoke::<settings::patch::Op>(&settings::patch::Input {
            changes: changes
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect(),
        })
        .await
        .unwrap();
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
    assert_eq!(updated_setting.source, SettingSource::Runtime);
    assert!(!updated_setting.is_default);
}
