//! One multiplexed SSE stream per browser origin.
//!
//! Browsers cap HTTP/1.1 at 6 connections per origin, and an EventSource holds
//! one for its whole life. The UI wants four different live streams — the fleet
//! layout, a session's event feed, its ACP chat deltas, and the operator log
//! tail — so a single tab spent 3 of the 6 slots and two tabs spent all of them.
//! Past the cap an ordinary `fetch()` never resolves: no error, no timeout, and
//! nothing in the server log, because the request never leaves the browser.
//!
//! `GET /api/events?topics=…` folds all of them onto one connection. Every frame
//! is the default `message` event carrying `{topic, event, data}`, so the client
//! routes on `topic` and recovers the original per-stream event name from
//! `event`. The per-stream routes stay as they are — they remain the documented
//! single-stream API, and this is the browser's connection-thrifty path.
//!
//! Authorization is deliberately *not* a new policy surface: each topic maps
//! back to the concrete route it replaces and is run through the same
//! [`grant_allows`] check the router applies, so a session-scoped credential
//! reaches exactly the topics whose routes it could already have called.

use std::convert::Infallible;
use std::pin::Pin;

use axum::{
    extract::{Query, State},
    http::{Method, StatusCode},
    response::sse::{self, KeepAlive, Sse},
    Extension,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt, StreamMap};

use crate::auth::Principal;
use crate::logs;

use super::auth::grant_allows;
use super::{require_branch, require_session, ApiResult, AppError, AppState};

/// A boxed per-topic stream. Every topic is erased to this so one [`StreamMap`]
/// can fold them together regardless of which broadcast they came from.
type TopicStream = Pin<Box<dyn Stream<Item = Result<sse::Event, Infallible>> + Send>>;

/// Cap on topics per connection. The point of this route is to spend one socket
/// instead of six; a client asking for hundreds of topics is a bug, and each one
/// costs a broadcast receiver.
const MAX_TOPICS: usize = 64;

/// `StreamMap` key for the never-yielding entry that keeps the response open.
/// Not a parseable topic name, so it cannot collide with a caller's topic.
const KEEPALIVE_KEY: &str = "__keepalive";

#[derive(Debug, Deserialize)]
pub(super) struct EventsQuery {
    /// Comma-separated topic list: `layout`, `logs`, `session:<key>`,
    /// `chat:<key>`.
    #[serde(default)]
    topics: String,
}

/// One frame on the multiplexed stream. `event` is the event name the
/// single-stream route would have used, so the client's per-topic handlers are
/// unchanged from the un-multiplexed shape.
#[derive(Debug, Serialize)]
struct Frame<'a> {
    topic: &'a str,
    event: &'a str,
    data: Value,
}

fn frame(topic: &str, event: &str, data: Value) -> Result<sse::Event, Infallible> {
    Ok(sse::Event::default()
        .json_data(Frame { topic, event, data })
        .unwrap_or_default())
}

/// The parsed form of one requested topic.
enum Topic {
    Layout,
    Logs,
    Session(String),
    Chat(String),
}

impl Topic {
    fn parse(raw: &str) -> Option<Self> {
        match raw.split_once(':') {
            Some(("session", key)) if !key.is_empty() => Some(Self::Session(key.to_string())),
            Some(("chat", key)) if !key.is_empty() => Some(Self::Chat(key.to_string())),
            Some(_) => None,
            None => match raw {
                "layout" => Some(Self::Layout),
                "logs" => Some(Self::Logs),
                _ => None,
            },
        }
    }

    /// The route this topic stands in for. Authorization runs against this path,
    /// so the multiplexed stream can never widen a credential's reach.
    fn route(&self) -> String {
        match self {
            Self::Layout => "/api/session-layout/events".to_string(),
            Self::Logs => "/api/logs/stream".to_string(),
            Self::Session(key) => format!("/api/sessions/{key}/events"),
            Self::Chat(key) => format!("/api/sessions/{key}/chat/stream"),
        }
    }
}

/// `GET /api/events?topics=layout,logs,session:abc,chat:abc` — every live stream
/// the caller asked for, folded onto one connection.
///
/// A topic the credential may not read fails the whole request: the client picks
/// its own topics, so that is a bug worth surfacing loudly rather than a stream
/// that silently omits a view's updates. A topic that merely fails to *resolve*
/// (an archived session the UI has not dropped yet) is non-fatal — it reports an
/// `error` frame on its own topic and the other topics keep flowing.
pub(super) async fn events_mux(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    let raw: Vec<&str> = q
        .topics
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if raw.len() > MAX_TOPICS {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            format!("too many topics ({}, max {MAX_TOPICS})", raw.len()),
        ));
    }

    // Keyed by topic, so a topic repeated in the query opens one receiver rather
    // than doubling every frame.
    let mut streams: StreamMap<String, TopicStream> = StreamMap::new();

    for name in raw {
        if streams.contains_key(name) {
            continue;
        }
        let topic = Topic::parse(name).ok_or_else(|| {
            AppError::new(StatusCode::BAD_REQUEST, format!("unknown topic '{name}'"))
        })?;
        if !grant_allows(&st, &principal, &Method::GET, &topic.route()).await {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                format!("credential grant forbids topic '{name}'"),
            ));
        }
        streams.insert(name.to_string(), topic_stream(&st, name, topic).await);
    }

    // Never let the merged stream terminate: an SSE response that ends puts the
    // browser into a reconnect loop, and `StreamMap` finishes once every entry
    // is exhausted. With no topics (or once every topic's broadcast closes) this
    // keeps the connection parked on keep-alive instead. `Topic::parse` rejects
    // this name, so it cannot collide with a real topic.
    streams.insert(KEEPALIVE_KEY.to_string(), Box::pin(tokio_stream::pending()));

    Ok(Sse::new(streams.map(|(_, event)| event)).keep_alive(KeepAlive::default()))
}

/// Build one topic's stream, already framed with its topic name.
async fn topic_stream(st: &AppState, name: &str, topic: Topic) -> TopicStream {
    let owned = name.to_string();
    match topic {
        Topic::Layout => {
            let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |result| {
                let event = result.ok()?;
                if event.kind != "session_layout" {
                    return None;
                }
                Some(frame(&owned, "session_layout", event.data))
            });
            Box::pin(stream)
        }
        Topic::Logs => {
            let stream =
                BroadcastStream::new(logs::buffer().subscribe()).filter_map(move |result| {
                    // A lagged subscriber yields Err; skip the gap (the client can
                    // re-snapshot).
                    let line = result.ok()?;
                    Some(frame(&owned, "log", json!(line)))
                });
            Box::pin(stream)
        }
        Topic::Session(key) => {
            let branch = match require_branch(&st.db, &key).await {
                Ok(branch) => branch,
                Err(e) => return unresolved(&owned, &e),
            };
            let id = branch.id;
            let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |result| {
                let event = result.ok()?;
                if event.branch_id != id {
                    return None;
                }
                let kind = event.kind.clone();
                Some(frame(&owned, &kind, json!(event)))
            });
            Box::pin(stream)
        }
        Topic::Chat(key) => {
            let session = match require_session(&st.db, &key).await {
                Ok((session, _)) => session,
                Err(e) => return unresolved(&owned, &e),
            };
            match st.acp.get(&session.id) {
                Some(handle) => {
                    let stream = BroadcastStream::new(handle.subscribe()).filter_map(move |r| {
                        let ev = r.ok()?;
                        Some(frame(&owned, &ev.event, ev.data))
                    });
                    Box::pin(stream)
                }
                // No live task yet: hold the topic open with no events, exactly
                // as the single-stream route does. A handoff installs a new task
                // and the client rebinds.
                None => Box::pin(tokio_stream::pending()),
            }
        }
    }
}

/// A topic that could not be resolved reports one `error` frame and then goes
/// quiet, leaving every other topic on the connection untouched.
fn unresolved(topic: &str, err: &AppError) -> TopicStream {
    let first = frame(topic, "error", json!({ "message": err.message() }));
    Box::pin(tokio_stream::once(first).chain(tokio_stream::pending()))
}
