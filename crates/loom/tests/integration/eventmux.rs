//! The multiplexed `events.stream` operation: many topics on one connection.
//!
//! The browser caps HTTP/1.1 at 6 connections per origin, so the UI folds every
//! live stream onto this one route. These cases pin the three properties that
//! make that safe: frames stay attributable to their topic, one bad topic does
//! not take the connection down with it, and multiplexing cannot widen what a
//! scoped credential may read.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::json;
use serial_test::serial;

use crate::acp::start_new;
use crate::fixtures::TestServer;

fn events_url(ts: &TestServer, topics: &str) -> String {
    format!(
        "http://{}/api/events/stream?topics={}",
        ts.addr,
        urlencoding_encode(topics)
    )
}

/// Percent-encode the few characters our topic lists actually use, so the tests
/// don't pull in a dependency for `,` and `:`.
fn urlencoding_encode(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            ':' => "%3A".to_string(),
            ',' => "%2C".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// Read the SSE body until `done` matches or the deadline passes.
async fn read_until(
    resp: reqwest::Response,
    timeout: Duration,
    done: impl Fn(&str) -> bool,
) -> String {
    let mut stream = resp.bytes_stream();
    let mut body = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if done(&body) {
                    break;
                }
            }
            _ => break,
        }
    }
    body
}

/// One connection carrying a session's chat topic delivers the same events the
/// dedicated `sessions.chat.stream` route would, each tagged with its topic so the
/// client can route it.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiplexed_stream_tags_frames_with_their_topic() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-mux", None, None).await;

    // Opening the stream subscribes the broadcasts before we prompt.
    let resp = reqwest::Client::new()
        .get(events_url(&ts, "chat:acp-mux,session:acp-mux"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    ts.client
        .post(
            "/api/sessions/acp-mux/prompt",
            json!({ "text": "say:multiplexed" }),
        )
        .await
        .unwrap();

    let body = read_until(resp, Duration::from_secs(10), |b| {
        b.contains("\"state\":\"ended\"")
    })
    .await;

    // Every frame is the default `message` event; the topic lives in the payload
    // so two sessions' `delta`s can share one connection.
    assert!(
        body.contains("\"topic\":\"chat:acp-mux\""),
        "frames carry their topic: {body}"
    );
    assert!(
        body.contains("\"event\":\"turn\""),
        "stream carried turn events: {body}"
    );
    assert!(
        body.contains("\"event\":\"block\""),
        "stream carried block events: {body}"
    );
    assert!(
        body.contains("\"event\":\"delta\""),
        "stream carried delta events: {body}"
    );
}

/// A topic name the server doesn't know is a client bug, not something to
/// silently ignore into a stream that looks alive but delivers nothing.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_topic_is_rejected() {
    let ts = TestServer::start().await;
    let resp = reqwest::Client::new()
        .get(events_url(&ts, "layout,not-a-topic"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A topic that cannot be resolved — an archived session the UI has not dropped
/// yet — must not take down the other topics sharing the connection. It reports
/// an error on its own topic and everything else keeps flowing.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unresolvable_topic_is_isolated_from_the_rest() {
    let ts = TestServer::start().await;
    start_new(&ts, "acp-live", None, None).await;

    let resp = reqwest::Client::new()
        .get(events_url(&ts, "session:no-such-session,chat:acp-live"))
        .send()
        .await
        .unwrap();
    // The connection itself is fine — the dead topic is reported in-band.
    assert_eq!(resp.status(), StatusCode::OK);

    ts.client
        .post(
            "/api/sessions/acp-live/prompt",
            json!({ "text": "say:still-flowing" }),
        )
        .await
        .unwrap();

    let body = read_until(resp, Duration::from_secs(10), |b| {
        b.contains("\"state\":\"ended\"")
    })
    .await;

    assert!(
        body.contains("\"topic\":\"session:no-such-session\"")
            && body.contains("\"event\":\"error\""),
        "dead topic reported an error frame: {body}"
    );
    assert!(
        body.contains("\"topic\":\"chat:acp-live\"") && body.contains("\"event\":\"delta\""),
        "the live topic kept streaming: {body}"
    );
}

/// Folding streams onto one route must not become a way around per-session
/// scoping: each topic is authorized against the route it stands in for.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_token_cannot_subscribe_to_another_session() {
    let ts = TestServer::start().await;
    let mine = ts
        .client
        .post(
            "/api/sessions",
            json!({ "cwd": ts.cwd(), "goal": "scoped", "agent": "shell" }),
        )
        .await
        .unwrap();
    let session_id = mine["id"].as_str().unwrap();
    let branch_id = mine["branch"]["id"].as_str().unwrap();
    let token =
        loom::auth::create_session_token(&ts.state.db, Some("rjpower"), session_id, branch_id)
            .await
            .unwrap();

    let theirs = ts
        .client
        .post(
            "/api/sessions",
            json!({ "cwd": ts.cwd(), "goal": "unrelated", "agent": "shell" }),
        )
        .await
        .unwrap();
    let other_id = theirs["id"].as_str().unwrap();

    let http = reqwest::Client::new();

    // Its own session's topic is exactly what `sessions.events.stream` allows.
    let own = http
        .get(events_url(&ts, &format!("session:{session_id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);

    // A sibling's is not — and asking for it alongside a permitted topic must
    // not smuggle it through.
    let other = http
        .get(events_url(
            &ts,
            &format!("session:{session_id},session:{other_id}"),
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::FORBIDDEN);

    // Human-only topics stay unavailable to scoped session credentials.
    let logs = http
        .get(events_url(&ts, "logs"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(logs.status(), StatusCode::FORBIDDEN);
}
