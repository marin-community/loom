//! One multiplexed SSE stream per browser origin.
//!
//! Browsers cap HTTP/1.1 at 6 connections per origin, and an EventSource holds
//! one for its whole life. The UI wants four different live streams — the fleet
//! layout, a session's event feed, its ACP chat deltas, and the operator log
//! tail — so a single tab spent 3 of the 6 slots and two tabs spent all of them.
//! Past the cap an ordinary `fetch()` never resolves: no error, no timeout, and
//! nothing in the server log, because the request never leaves the browser.
//!
//! `events.stream` (`GET /api/events/stream?topics=…`) folds all of them onto one
//! connection. Every frame is the default `message` event carrying
//! `{topic, event, data}`, so the client routes on `topic` and recovers the
//! original per-stream event name from `event`. The single-topic operations
//! (`session_layout.events`, `logs.stream`, `sessions.events.stream`,
//! `sessions.chat.stream`) remain the documented one-stream-per-connection API;
//! this is the browser's connection-thrifty path over the same feeds.
//!
//! Authorization is deliberately *not* a new policy surface. `events.stream` is
//! a container: reaching it grants nothing, and each topic is authorized against
//! the *declaration* of the single-topic operation it stands in for — the same
//! actor policy, grants, and `Scoped` resource check that operation gets when
//! called directly. So a credential reaches exactly the topics it could already
//! have opened one at a time, and adding a topic means naming its operation
//! rather than editing a path allowlist.

use std::convert::Infallible;
use std::pin::Pin;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::sse::{self, KeepAlive, Sse},
    Extension,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt, StreamMap};

use weaver_api::operations::{events, logs as log_operations, session_layout, sessions};

use crate::auth::Principal;
use crate::logs;

use super::streams::authorized;
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

pub(super) fn chat_event_parts(
    result: Result<crate::acp::SseEvent, BroadcastStreamRecvError>,
) -> (String, Value) {
    match result {
        Ok(event) => (event.event, event.data),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            // A dropped chat frame can hide prose or a turn boundary
            // indefinitely. Tell the browser to reconcile from the durable
            // journal; silently skipping gives it no reason to repair itself.
            ("resync".to_string(), json!({ "skipped": skipped }))
        }
    }
}

fn chat_frame(
    topic: &str,
    result: Result<crate::acp::SseEvent, BroadcastStreamRecvError>,
) -> Result<sse::Event, Infallible> {
    let (event, data) = chat_event_parts(result);
    frame(topic, &event, data)
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

    /// Authorize this topic as the operation it stands in for.
    ///
    /// The stand-in is the whole mechanism: `layout` is `session_layout.events`,
    /// `session:<key>` is `sessions.events.stream` on that key, and so on. Each
    /// gets its own declaration's actor policy, grants, and resource scope, so
    /// the multiplexed stream cannot widen a credential's reach — and a topic
    /// added here without an operation behind it does not compile.
    async fn authorize(&self, st: &AppState, principal: &Principal) -> ApiResult<()> {
        match self {
            Self::Layout => {
                authorized::<session_layout::events::Events>(st, principal, Default::default())
                    .await?;
            }
            Self::Logs => {
                authorized::<log_operations::stream::Stream>(st, principal, Default::default())
                    .await?;
            }
            Self::Session(key) => {
                authorized::<sessions::events::stream::Stream>(
                    st,
                    principal,
                    sessions::events::stream::Input {
                        session: key.clone(),
                    },
                )
                .await?;
            }
            Self::Chat(key) => {
                authorized::<sessions::chat::stream::Stream>(
                    st,
                    principal,
                    sessions::chat::stream::Input {
                        session: key.clone(),
                    },
                )
                .await?;
            }
        }
        Ok(())
    }
}

/// The `events.stream` operation — `?topics=layout,logs,session:abc,chat:abc`,
/// every live stream the caller asked for folded onto one connection.
///
/// A topic the credential may not read fails the whole request: the client picks
/// its own topics, so that is a bug worth surfacing loudly rather than a stream
/// that silently omits a view's updates. A topic that merely fails to *resolve*
/// (an archived session the UI has not dropped yet) is non-fatal — it reports an
/// `error` frame on its own topic and the other topics keep flowing.
pub(super) async fn events_mux(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<events::stream::Input>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    let input = authorized::<events::stream::Stream>(&st, &principal, input).await?;
    let raw: Vec<&str> = input
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
        // A topic the credential may not read fails the whole request, with the
        // refusing operation's own message rather than a generic one.
        topic.authorize(&st, &principal).await.map_err(|error| {
            AppError::new(error.status, format!("topic '{name}': {}", error.message()))
        })?;
        streams.insert(
            name.to_string(),
            topic_stream(&st, &principal, name, topic).await?,
        );
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
async fn topic_stream(
    st: &AppState,
    principal: &Principal,
    name: &str,
    topic: Topic,
) -> ApiResult<TopicStream> {
    let owned = name.to_string();
    let stream: TopicStream = match topic {
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
            let redactor = super::logview::log_redactor(&st.db, principal).await?;
            let stream =
                BroadcastStream::new(logs::buffer().subscribe()).filter_map(move |result| {
                    // A lagged subscriber yields Err; skip the gap (the client can
                    // re-snapshot).
                    let line = super::logview::redact_line(&redactor, result.ok()?);
                    Some(frame(&owned, "log", json!(line)))
                });
            Box::pin(stream)
        }
        Topic::Session(key) => {
            let branch = match require_branch(&st.db, &key).await {
                Ok(branch) => branch,
                Err(e) => return Ok(unresolved(&owned, &e)),
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
                Err(e) => return Ok(unresolved(&owned, &e)),
            };
            match st.acp.get(&session.id) {
                Some(handle) => {
                    let stream = BroadcastStream::new(handle.subscribe())
                        .map(move |result| chat_frame(&owned, result));
                    Box::pin(stream)
                }
                // No live task yet: hold the topic open with no events, exactly
                // as the single-stream route does. A handoff installs a new task
                // and the client rebinds.
                None => Box::pin(tokio_stream::pending()),
            }
        }
    };
    Ok(stream)
}

/// A topic that could not be resolved reports one `error` frame and then goes
/// quiet, leaving every other topic on the connection untouched.
fn unresolved(topic: &str, err: &AppError) -> TopicStream {
    let first = frame(topic, "error", json!({ "message": err.message() }));
    Box::pin(tokio_stream::once(first).chain(tokio_stream::pending()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_lag_is_an_explicit_resync_frame() {
        let (event, data) = chat_event_parts(Err(BroadcastStreamRecvError::Lagged(7)));
        assert_eq!(event, "resync");
        assert_eq!(data, json!({ "skipped": 7 }));
    }
}
