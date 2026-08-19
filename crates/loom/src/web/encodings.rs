//! The registered operations whose wire encoding is not JSON.
//!
//! `io` changes exactly one thing about an operation: how the request or the
//! response is encoded. The declaration, the actor policy, the grants, the
//! resource scope, and the operands are all read from the registry the same way
//! a JSON operation's are — these handlers exist because axum needs a concrete
//! response type for an SSE body, a websocket upgrade, or a byte body, not
//! because the operation sits outside the model.
//!
//! Four encodings live here:
//!
//! | `io` | why the handler is custom |
//! |---|---|
//! | `Stream` | the response is an SSE body |
//! | `Duplex` | the response is a websocket upgrade |
//! | `Upload` | the *request* body is the payload's raw bytes |
//! | `Download` | the *response* body is raw bytes plus a content type |
//!
//! Two consequences worth stating, because getting either wrong is how the
//! previous surface drifted:
//!
//! * **The route is derived, not written.** [`mount`] walks the registry and
//!   mounts every non-JSON operation at `spec.path()`. That is why a session
//!   stream takes `?session=…` rather than a path segment: an operand is an
//!   operand regardless of encoding, and a path parameter would make the real
//!   route unequal to the declared one. It is also why every operand of an
//!   operation mounted here is optional on the wire — the query string is
//!   extracted before any default-filling could run.
//! * **Authorization is the dispatcher's, not the handler's.** Every handler
//!   calls [`super::operations::authorize_declared`], which is the same context
//!   fill and the same `authorize` the JSON dispatcher runs. Actor policy alone
//!   is also checked upstream in [`super::auth::grant_allows`]; what only this
//!   call supplies is the `Scoped` resource check, so a stream cannot quietly
//!   skip the rule that a session credential reaches only its own session tree.

use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, FromRef, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Extension, Router};
use weaver_api::operations::{artifacts, sessions, shell, Operation, Scoped};

use crate::auth::Principal;
use crate::{AppState, EditorState};

use super::operations::{self as ops, Bound, OperationContext};
use super::ApiResult;

/// Mount every non-JSON operation at its derived route.
///
/// `io = Session` is excluded: those three are JSON-bodied and mounted beside
/// the auth routes because their response must carry a `Set-Cookie`, which is a
/// header concern rather than an encoding one. Everything else reaching this
/// match is a stream, a websocket, or a byte body, and the ids it accepts are
/// the whole list — `mounted_encodings` is that same match read back, so the
/// test below cannot pass by describing a different one.
pub(super) fn mount(router: Router<AppState>) -> Router<AppState> {
    mount_inner(router).0
}

fn mount_inner(router: Router<AppState>) -> (Router<AppState>, Vec<&'static str>) {
    let mut router = router;
    let mut mounted = Vec::new();
    for operation in weaver_api::operations() {
        let handler: routing::MethodRouter<AppState> = match operation.id {
            "events.stream" => routing::get(super::eventmux::events_mux),
            "logs.stream" => routing::get(super::logview::logs_stream),
            "session_layout.events" => routing::get(super::session_layout::session_layout_events),
            "sessions.events.stream" => routing::get(super::sessions::events_sse),
            "sessions.chat.stream" => routing::get(super::sessions::chat_stream),
            "sessions.terminal" => routing::get(session_terminal),
            "sessions.shells.terminal" => routing::get(session_shell_terminal),
            "shell.terminal" => routing::get(shell_terminal),
            "sessions.raw" => routing::get(session_raw_file),
            "artifacts.raw" => routing::get(artifact_raw_image),
            // The body limit is the scratch file cap, not the JSON cap: this is
            // the one route where a large body is the point.
            "sessions.scratch.write" => routing::post(write_scratch_file).layer(
                DefaultBodyLimit::max(crate::scratch::MAX_SCRATCH_FILE_BYTES),
            ),
            _ => continue,
        };
        let path = operation
            .path()
            .strip_prefix("/api")
            .unwrap_or_default()
            .to_string();
        router = router.route(&path, handler);
        mounted.push(operation.id);
    }
    (router, mounted)
}

/// The ids [`mount`] actually served, in registry order.
#[cfg(test)]
fn mounted_encodings() -> Vec<&'static str> {
    mount_inner(Router::new()).1
}

/// `shell.restart` is ordinary JSON; it is bound here because its sibling
/// `shell.terminal` is a websocket and the bundle should have one home.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![ops::register::<shell::restart::Restart, _, _>(
        restart_shell,
    )]
}

async fn restart_shell(
    context: OperationContext,
    _input: shell::restart::Input,
) -> ApiResult<weaver_api::dto::ShellRestartResult> {
    crate::shell::restart(&context.state).await?;
    Ok(weaver_api::dto::ShellRestartResult { restarted: true })
}

// ---------------------------------------------------------------------------
// Byte bodies. `Download` answers an `<img src>` or a download link, `Upload`
// takes a file as the request body. What belongs here is the operand extraction
// and the authorization; the bytes themselves are the resource module's job, the
// same division the websockets below follow.
// ---------------------------------------------------------------------------

async fn session_raw_file(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<sessions::raw::Input>,
) -> ApiResult<Response> {
    let input = authorized::<sessions::raw::Raw>(&st, &principal, input).await?;
    super::sessions::raw_session_bytes(&st, &input).await
}

async fn artifact_raw_image(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<artifacts::raw::Input>,
) -> ApiResult<Response> {
    let input = authorized::<artifacts::raw::Raw>(&st, &principal, input).await?;
    super::artifacts::raw_artifact_bytes(&st, &input).await
}

async fn write_scratch_file(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<sessions::scratch::write::Input>,
    body: Bytes,
) -> ApiResult<axum::Json<weaver_api::dto::ScratchWriteResult>> {
    let input = authorized::<sessions::scratch::write::Write>(&st, &principal, input).await?;
    super::scratch::write_scratch_bytes(&st, &input, &body)
        .await
        .map(axum::Json)
}

// ---------------------------------------------------------------------------
// Websockets. The byte pump and the Origin check live in `loom-editor`; what
// belongs here is the operand extraction and the authorization, so that crate
// stays a transport with no view of the registry.
// ---------------------------------------------------------------------------

async fn session_terminal(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<sessions::terminal::Input>,
    headers: HeaderMap,
) -> Response {
    let input = match authorized::<sessions::terminal::Terminal>(&st, &principal, input).await {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    let editor = EditorState::from_ref(&st);
    crate::terminal::terminal_ws(ws, &editor, &input.session, &headers).await
}

async fn session_shell_terminal(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<sessions::shells::terminal::Input>,
    headers: HeaderMap,
) -> Response {
    let input =
        match authorized::<sessions::shells::terminal::Terminal>(&st, &principal, input).await {
            Ok(input) => input,
            Err(error) => return error.into_response(),
        };
    let editor = EditorState::from_ref(&st);
    crate::terminal::session_shell_ws(ws, &editor, &input.session, input.index, &headers).await
}

async fn shell_terminal(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<shell::terminal::Input>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authorized::<shell::terminal::Terminal>(&st, &principal, input).await {
        return error.into_response();
    }
    let editor = EditorState::from_ref(&st);
    crate::terminal::shell_ws(ws, &editor, &headers).await
}

/// Run the registry's authorization over declared operands, yielding the input
/// with dispatcher-supplied context filled in.
pub(super) async fn authorized<O>(
    st: &AppState,
    principal: &Principal,
    mut input: O::Input,
) -> ApiResult<O::Input>
where
    O: Operation,
    O::Input: Scoped,
{
    ops::authorize_declared::<O>(st, principal, &mut input).await?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use axum::extract::Query;
    use weaver_api::operations::{
        artifacts, events, logs, session_layout, sessions, shell, Operation,
    };

    /// A non-JSON operation's operands arrive in the query string, and axum's
    /// `Query` runs before any dispatcher default-filling could. So every operand
    /// of one must be optional on the wire — otherwise the declared
    /// route 400s on a request that named nothing, including exactly the request
    /// a session credential makes when it means "my own session".
    ///
    /// Checked through the real extractor rather than through `serde_json`,
    /// because urlencoded and JSON disagree about what a missing field is.
    #[test]
    fn non_json_operations_take_every_operand_from_the_query_string() {
        fn check<O: Operation>() {
            let uri: axum::http::Uri = "/whatever".parse().expect("static uri");
            Query::<O::Input>::try_from_uri(&uri)
                .map(|_| ())
                .unwrap_or_else(|error| {
                    panic!(
                        "{} has an operand a bare `?` cannot supply: {error}. \
                     A stream's operands come from the query string, so each one \
                     needs #[serde(default)].",
                        O::SPEC.id
                    )
                });
        }

        check::<events::stream::Stream>();
        check::<logs::stream::Stream>();
        check::<session_layout::events::Events>();
        check::<sessions::events::stream::Stream>();
        check::<sessions::chat::stream::Stream>();
        check::<sessions::terminal::Terminal>();
        check::<sessions::shells::terminal::Terminal>();
        check::<shell::terminal::Terminal>();
        check::<sessions::raw::Raw>();
        check::<sessions::scratch::write::Write>();
        check::<artifacts::raw::Raw>();
    }

    /// The other half: an operand a caller *does* name arrives intact.
    ///
    /// "Every field is optional" is satisfied by a struct that silently ignores
    /// everything, so optionality alone proves nothing. These are the exact URLs
    /// the frontend and the integration tests build, including the one operand
    /// that is not a string — `index` on a debug shell has to survive the
    /// urlencoded round trip as a `u32`, and it is the only non-string operand on
    /// any stream.
    #[test]
    fn a_named_operand_reaches_the_handler() {
        let uri: axum::http::Uri = "/api/sessions/terminal?session=abc123"
            .parse()
            .expect("static uri");
        let Query(input) = Query::<sessions::terminal::Input>::try_from_uri(&uri)
            .expect("a session id in the query string");
        assert_eq!(input.session, "abc123");

        let uri: axum::http::Uri = "/api/sessions/shells/terminal?session=abc123&index=2"
            .parse()
            .expect("static uri");
        let Query(input) = Query::<sessions::shells::terminal::Input>::try_from_uri(&uri)
            .expect("a session id and a shell index in the query string");
        assert_eq!((input.session.as_str(), input.index), ("abc123", 2));

        let uri: axum::http::Uri = "/api/events/stream?topics=layout,logs,session:abc"
            .parse()
            .expect("static uri");
        let Query(input) = Query::<events::stream::Input>::try_from_uri(&uri)
            .expect("a comma-separated topic list");
        assert_eq!(input.topics, "layout,logs,session:abc");
    }

    /// Every non-JSON operation is mounted, at its own derived path.
    ///
    /// `mount` skips an id it does not recognize, which is right for
    /// `io = Session` and wrong for an encoding someone declared and forgot to
    /// serve. This is the check that tells them apart — and building the router
    /// is also what proves no two of them claim one path.
    #[test]
    fn every_non_json_operation_is_mounted() {
        let declared: Vec<&str> = weaver_api::operations()
            .filter(|operation| {
                matches!(
                    operation.io.as_str(),
                    "stream" | "duplex" | "upload" | "download"
                )
            })
            .map(|operation| operation.id)
            .collect();
        assert!(!declared.is_empty(), "no non-JSON operations are declared");
        assert_eq!(
            declared,
            super::mounted_encodings(),
            "declared non-JSON operations that `mount` does not serve"
        );
    }
}
