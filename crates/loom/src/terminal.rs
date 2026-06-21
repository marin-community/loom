//! WebSocket ⇄ terminal bridge: the browser's xterm.js talks to the session's
//! [`tapestry`] supervisor, which streams raw PTY bytes — so the browser gets a
//! real terminal (colour, cursor, mouse, and xterm's own scrollback, selection,
//! and search) instead of a polled read-only mirror.
//!
//! This is a parallel, purely-interactive channel; the hooks → events → SSE data
//! plane and the monitor heartbeat are untouched.
//!
//! ## Wire protocol
//!
//! Binary frames both directions. server → client is raw PTY output bytes. The
//! client → server hot path is opcode-prefixed (the ttyd/terminado/VS Code
//! shape):
//!
//! * `0x00 <bytes…>` — keystrokes, forwarded verbatim to the PTY writer.
//! * `0x01 <cols:u16_be> <rows:u16_be>` — resize (exactly 5 bytes).
//!
//! Text frames are tolerated as raw keystrokes (no opcode); the frontend always
//! sends binary. Empty frames, malformed resizes, and unknown opcodes are
//! dropped.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header::{HOST, ORIGIN};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};

use crate::backend;
use crate::web::{require_session, AppState};

/// Cap on a single inbound WebSocket frame. Keystrokes and resizes are tiny and
/// even a large paste is bounded; this stops a hostile client forcing a huge
/// allocation before we inspect the opcode.
const MAX_FRAME: usize = 64 * 1024;

const OP_INPUT: u8 = 0x00;
const OP_RESIZE: u8 = 0x01;

/// `GET /api/sessions/{id}/terminal` — upgrade to a WebSocket bridged to the
/// session's terminal supervisor.
pub async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    // CSWSH defence: CORS does NOT apply to WebSockets, so a localhost bind is
    // not protection — validate the Origin before upgrading. See `origin_ok`.
    // `auth.base_url` (when set) is the operator-declared canonical origin and is
    // trusted directly; absent that, only loopback or a *proxied* same-Host
    // Origin is accepted.
    let base_url = crate::config::get(&st.db, "auth.base_url").await;
    if !origin_ok(&headers, &st.addr, base_url.as_deref()) {
        // This rejection used to be silent — invisible behind a reverse proxy,
        // where it surfaces only as the browser's endless "reconnecting". Log it
        // with the Origin/Host so a proxy misconfig is one `docker logs` away.
        let origin = header_str(&headers, ORIGIN);
        let host = header_str(&headers, HOST);
        tracing::warn!(
            session = %key,
            origin = %origin,
            host = %host,
            bound = %st.addr,
            base_url = base_url.as_deref().unwrap_or("<unset>"),
            "terminal websocket rejected: Origin is not loopback, the configured \
             auth.base_url, nor a proxied (X-Forwarded-*) request's own Host"
        );
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let session = match require_session(&st.db, &key).await {
        Ok((s, _)) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "no such session").into_response(),
    };
    // No live supervisor means nothing to attach to — the caller should adopt first.
    if !backend::has_session(&session.term_session).await {
        return (
            StatusCode::CONFLICT,
            "session has no running terminal — adopt it first",
        )
            .into_response();
    }
    let target = session.term_session.clone();
    tracing::info!(session = %key, target = %target, "terminal websocket attached");
    upgrade_to_bridge(ws, target)
}

/// `GET /api/shell/terminal` — upgrade to a WebSocket bridged to the operator
/// scratch shell (see [`crate::shell`]). Same Origin check and byte pump as
/// [`terminal_ws`], but there is no session to look up: the shell is a single
/// fixed supervisor, spawned lazily here on first attach so the UI can connect
/// without a separate "create" step.
pub async fn shell_ws(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let base_url = crate::config::get(&st.db, "auth.base_url").await;
    if !origin_ok(&headers, &st.addr, base_url.as_deref()) {
        let origin = header_str(&headers, ORIGIN);
        let host = header_str(&headers, HOST);
        tracing::warn!(
            origin = %origin,
            host = %host,
            bound = %st.addr,
            "shell websocket rejected: Origin is not loopback, the configured \
             auth.base_url, nor a proxied (X-Forwarded-*) request's own Host"
        );
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    if let Err(e) = crate::shell::ensure(&st).await {
        tracing::error!(error = %e, "failed to bring up operator scratch shell");
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not start shell").into_response();
    }
    let target = crate::shell::SHELL_SESSION.to_string();
    tracing::info!(target = %target, "shell websocket attached");
    upgrade_to_bridge(ws, target)
}

/// `GET /api/sessions/{id}/shell/{idx}/terminal` — upgrade to a WebSocket bridged
/// to one of the session's worktree **debug shells** (see [`crate::shell`]). Same
/// Origin check and byte pump as [`terminal_ws`], but the target is a plain login
/// shell *in the session's worktree*, spawned lazily here on first attach — not
/// the agent itself. Multiple indices give multiple concurrent shells.
pub async fn session_shell_ws(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Path((key, idx)): Path<(String, u32)>,
    headers: HeaderMap,
) -> Response {
    let base_url = crate::config::get(&st.db, "auth.base_url").await;
    if !origin_ok(&headers, &st.addr, base_url.as_deref()) {
        let origin = header_str(&headers, ORIGIN);
        let host = header_str(&headers, HOST);
        tracing::warn!(
            session = %key,
            origin = %origin,
            host = %host,
            bound = %st.addr,
            "session shell websocket rejected: Origin is not loopback, the \
             configured auth.base_url, nor a proxied (X-Forwarded-*) request's \
             own Host"
        );
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let (session, branch) = match require_session(&st.db, &key).await {
        Ok(pair) => pair,
        Err(_) => return (StatusCode::NOT_FOUND, "no such session").into_response(),
    };
    let target = match crate::shell::ensure_debug(&st, &session, &branch, idx).await {
        Ok(name) => name,
        Err(e) => {
            tracing::error!(session = %key, idx, error = %e, "failed to bring up session debug shell");
            return (StatusCode::INTERNAL_SERVER_ERROR, "could not start shell").into_response();
        }
    };
    tracing::info!(session = %key, target = %target, "session debug shell attached");
    upgrade_to_bridge(ws, target)
}

/// The shared tail of every terminal/shell handler: bound the inbound frame size
/// (keystrokes and resizes are tiny — see [`MAX_FRAME`]), then upgrade and
/// byte-pump the socket against `target`'s supervisor until either side closes.
/// `target` already names the supervisor in the logs, so the bridge's own lines
/// don't requalify it as "terminal"/"shell".
fn upgrade_to_bridge(ws: WebSocketUpgrade, target: String) -> Response {
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            if let Err(e) = bridge(socket, target.clone()).await {
                tracing::debug!(target = %target, error = %e, "terminal bridge ended with error");
            } else {
                tracing::debug!(target = %target, "terminal bridge detached cleanly");
            }
        })
}

/// Bridge a browser xterm to a session's [`tapestry`] supervisor: the supervisor
/// streams raw PTY bytes, so this is a straight byte pump. Dropping the input
/// half closes the socket write half, which the supervisor sees as a clean
/// detach — the child keeps running, and a refresh reconnects and repaints.
async fn bridge(socket: WebSocket, target: String) -> anyhow::Result<()> {
    let client = tapestry::Client::connect(&target).await?;
    // Start at 80×24; the client sends a real size in its first fit.
    let attach = client.attach(80, 24).await?;
    let (mut input, mut output) = attach.split();
    let (mut sink, mut stream) = socket.split();

    // PTY output → ws.
    let mut out_task = tokio::spawn(async move {
        while let Some(chunk) = output.recv().await {
            if sink.send(Message::Binary(chunk.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // ws → PTY input / resize.
    let mut in_task = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Binary(payload)) => {
                    if payload.is_empty() {
                        continue;
                    }
                    match payload[0] {
                        OP_INPUT => {
                            if input.send_input(&payload[1..]).await.is_err() {
                                break;
                            }
                        }
                        OP_RESIZE if payload.len() == 5 => {
                            let cols = u16::from_be_bytes([payload[1], payload[2]]).clamp(1, 1000);
                            let rows = u16::from_be_bytes([payload[3], payload[4]]).clamp(1, 1000);
                            if input.resize(cols, rows).await.is_err() {
                                break;
                            }
                        }
                        _ => continue, // malformed resize / unknown opcode
                    }
                }
                Ok(Message::Text(t)) => {
                    if input.send_input(t.as_str().as_bytes()).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue, // Ping/Pong handled by axum
            }
        }
    });

    // Whichever pump finishes first, drop the other — dropping `input` closes the
    // socket write half, which the supervisor sees as a clean detach (the child
    // keeps running, a refresh reconnects and repaints).
    tokio::select! {
        _ = &mut out_task => in_task.abort(),
        _ = &mut in_task => out_task.abort(),
    }
    Ok(())
}

/// Whether to allow the WebSocket upgrade.
///
/// A *missing* Origin (non-browser clients: the CLI, tests, `tokio-tungstenite`)
/// is allowed — browsers always send Origin on a WS handshake, so this does not
/// weaken the browser-CSWSH defence (it only waives same-host non-browser
/// processes, acceptable under the single-user-localhost assumption).
///
/// A *present* Origin is accepted when any of:
///
/// 1. it is a loopback host on our actual bound port — direct localhost access
///    (`loom open`, an SSH tunnel), the original single-user posture; or
/// 2. it matches the operator-configured `auth.base_url` (the canonical public
///    origin) — trusted however the request arrived, so it also works behind a
///    proxy that strips `X-Forwarded-*`; or
/// 3. it equals the request's own `Host` *and* the request carries an
///    `X-Forwarded-*` header — i.e. the browser is talking to the same public
///    origin the front-door proxy serves us under (`weaver.example.com`), and
///    the request demonstrably transited that proxy. Behind Caddy the loopback
///    rule never matches (the Origin is the public HTTPS host), so without 2/3
///    the in-browser terminal wedges on an endless "reconnecting". Rule 3 keeps
///    multi-vhost deploys zero-config (no need to enumerate every hostname).
///
/// Why rule 3 is safe where a bare `Origin == Host` would not be: trusting the
/// raw `Host` reopens the DNS-rebinding vector the loopback pin closed (a
/// rebinding page can force `Origin == Host` against a directly bound daemon).
/// Gating on `X-Forwarded-*` closes it: a reverse proxy adds those headers, and
/// a browser's WebSocket handshake — the attack vector — cannot set request
/// headers, so a rebinding page reaching a directly bound daemon has none and is
/// rejected. (Non-browser clients omit Origin entirely and are handled above.)
fn origin_ok(headers: &HeaderMap, bound_addr: &str, base_url: Option<&str>) -> bool {
    let origin = match headers.get(ORIGIN) {
        // Non-browser clients (the CLI, tests, tokio-tungstenite) omit Origin.
        None => return true,
        Some(v) => match v.to_str() {
            Ok(o) => o,
            Err(_) => return false,
        },
    };
    if origin_is_loopback(origin, bound_addr) {
        return true;
    }
    // (a) The operator-declared canonical origin (`auth.base_url`) is trusted
    // however the request reached us — covers a proxy that strips X-Forwarded-*.
    if let Some(authority) = base_url.and_then(url_authority) {
        if origin_matches_host(origin, authority) {
            return true;
        }
    }
    // (b) Otherwise trust an Origin equal to the request's own Host ONLY when the
    // request demonstrably transited the reverse proxy (it carries X-Forwarded-*).
    // This keeps multi-vhost proxied deploys zero-config while closing the
    // DNS-rebinding / CSWSH vector the loopback pin guarded: a browser-driven
    // handshake (the attack vector) cannot set X-Forwarded-* — the WebSocket API
    // forbids custom request headers — so a rebinding page reaching a directly
    // bound daemon yields Origin == Host but no forwarded header, and is rejected.
    if via_trusted_proxy(headers) {
        if let Some(host) = headers.get(HOST).and_then(|v| v.to_str().ok()) {
            return origin_matches_host(origin, host);
        }
    }
    false
}

/// The host[:port] authority of a `http(s)://` URL (e.g. `auth.base_url`), or
/// `None` if it isn't an http(s) URL. Trailing path/slash is dropped.
fn url_authority(url: &str) -> Option<&str> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    Some(rest.split('/').next().unwrap_or(rest))
}

/// True iff the request carries a header a reverse proxy adds (`X-Forwarded-*`).
/// Caddy/nginx set these; a browser's WebSocket handshake cannot, so their
/// presence is evidence the request reached us *through* the proxy rather than
/// from a page that rebound a hostname to a directly bound daemon.
fn via_trusted_proxy(headers: &HeaderMap) -> bool {
    headers.contains_key("x-forwarded-for")
        || headers.contains_key("x-forwarded-proto")
        || headers.contains_key("x-forwarded-host")
}

/// Read a header as a `&str`, or `"<none>"` when absent/non-ASCII. For logging.
fn header_str(headers: &HeaderMap, name: axum::http::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<none>")
}

/// Split an authority (`host`, `host:port`, `[::1]`, or `[::1]:port`) into its
/// host and optional port. Shared by the loopback and proxied-Host checks so
/// both parse IPv6 literals identically.
fn split_authority(authority: &str) -> (&str, Option<&str>) {
    if let Some(after) = authority.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:port
        match after.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':')),
            None => (authority, None),
        }
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        (h, Some(p))
    } else {
        (authority, None)
    }
}

/// True iff `origin` (`http(s)://host[:port]`) names the same host+port as the
/// request's `Host` header. The browser sends the page's own origin on a WS
/// handshake, so behind a proxy serving `https://h/` the Origin is `https://h`
/// and the Host is `h` — they agree once default ports (443/80) are filled in.
fn origin_matches_host(origin: &str, host: &str) -> bool {
    let (scheme, rest) = if let Some(r) = origin.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = origin.strip_prefix("http://") {
        ("http", r)
    } else {
        return false;
    };
    let origin_authority = rest.split('/').next().unwrap_or(rest);
    let (o_host, o_port) = split_authority(origin_authority);
    let (h_host, h_port) = split_authority(host);
    // DNS hostnames are case-insensitive, so compare them that way — else a
    // browser Origin and proxy Host differing only in ASCII case falsely reject.
    if o_host.is_empty() || !o_host.eq_ignore_ascii_case(h_host) {
        return false;
    }
    let default = if scheme == "https" { "443" } else { "80" };
    o_port.unwrap_or(default) == h_port.unwrap_or(default)
}

/// True iff `origin` is `http(s)://<loopback>[:<port>]` whose port equals the
/// port of `bound_addr`.
fn origin_is_loopback(origin: &str, bound_addr: &str) -> bool {
    let Some((_, want_port)) = bound_addr.rsplit_once(':') else {
        return false;
    };
    let (scheme, rest) = if let Some(r) = origin.strip_prefix("http://") {
        ("http", r)
    } else if let Some(r) = origin.strip_prefix("https://") {
        ("https", r)
    } else {
        return false;
    };
    // Origin carries no path, but be defensive about a trailing slash.
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = split_authority(authority);
    let port = port.unwrap_or(if scheme == "https" { "443" } else { "80" });
    if port != want_port {
        return false;
    }
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

#[cfg(test)]
mod tests {
    use super::{
        origin_is_loopback, origin_matches_host, origin_ok, url_authority, via_trusted_proxy,
    };
    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn accepts_loopback_origins_on_the_bound_port() {
        assert!(origin_is_loopback(
            "http://127.0.0.1:7878",
            "127.0.0.1:7878"
        ));
        assert!(origin_is_loopback(
            "http://localhost:7878",
            "127.0.0.1:7878"
        ));
        assert!(origin_is_loopback("http://[::1]:7878", "127.0.0.1:7878"));
        // Bound to all interfaces but the browser reached us via loopback.
        assert!(origin_is_loopback("http://localhost:9999", "0.0.0.0:9999"));
    }

    #[test]
    fn rejects_wrong_port() {
        assert!(!origin_is_loopback(
            "http://127.0.0.1:1234",
            "127.0.0.1:7878"
        ));
        // No port on the origin defaults to 80/443, which won't match.
        assert!(!origin_is_loopback("http://127.0.0.1", "127.0.0.1:7878"));
    }

    #[test]
    fn rejects_non_loopback_host() {
        // The DNS-rebinding vector: attacker.com rebound to 127.0.0.1 sends
        // Origin: http://attacker.com:7878 — not in the loopback set → rejected.
        assert!(!origin_is_loopback(
            "http://attacker.com:7878",
            "127.0.0.1:7878"
        ));
        assert!(!origin_is_loopback(
            "http://192.168.1.5:7878",
            "0.0.0.0:7878"
        ));
    }

    #[test]
    fn rejects_non_http_scheme_and_garbage() {
        assert!(!origin_is_loopback(
            "ftp://127.0.0.1:7878",
            "127.0.0.1:7878"
        ));
        assert!(!origin_is_loopback("null", "127.0.0.1:7878"));
        assert!(!origin_is_loopback("", "127.0.0.1:7878"));
    }

    #[test]
    fn accepts_origin_matching_proxied_host() {
        // Behind Caddy: browser at https://weaver.rjp.io, proxy forwards
        // Host: weaver.rjp.io. Default https port (443) fills in on both sides.
        assert!(origin_matches_host(
            "https://weaver.rjp.io",
            "weaver.rjp.io"
        ));
        assert!(origin_matches_host("https://loom.rjp.io", "loom.rjp.io"));
        // Explicit matching ports also agree.
        assert!(origin_matches_host(
            "https://h.example:8443",
            "h.example:8443"
        ));
        // Plain http with the default port elided on the Origin side.
        assert!(origin_matches_host("http://box.local", "box.local:80"));
    }

    #[test]
    fn rejects_origin_not_matching_host() {
        // A different host (the cross-site attacker case) never matches.
        assert!(!origin_matches_host(
            "https://attacker.example",
            "weaver.rjp.io"
        ));
        // Same host, mismatched explicit port.
        assert!(!origin_matches_host(
            "https://weaver.rjp.io:1234",
            "weaver.rjp.io"
        ));
        // Non-http scheme and garbage.
        assert!(!origin_matches_host("ftp://weaver.rjp.io", "weaver.rjp.io"));
        assert!(!origin_matches_host("null", "weaver.rjp.io"));
    }

    #[test]
    fn host_match_is_case_insensitive() {
        // DNS is case-insensitive; an Origin/Host casing difference must not reject.
        assert!(origin_matches_host(
            "https://Weaver.RJP.io",
            "weaver.rjp.io"
        ));
        assert!(origin_matches_host(
            "https://weaver.rjp.io",
            "WEAVER.RJP.IO"
        ));
    }

    #[test]
    fn url_authority_extracts_host_port() {
        assert_eq!(url_authority("https://loom.rjp.io"), Some("loom.rjp.io"));
        assert_eq!(url_authority("https://loom.rjp.io/"), Some("loom.rjp.io"));
        assert_eq!(
            url_authority("http://h.example:8443/x"),
            Some("h.example:8443")
        );
        assert_eq!(url_authority("ws://loom.rjp.io"), None); // not http(s)
        assert_eq!(url_authority(""), None);
    }

    #[test]
    fn via_trusted_proxy_detects_forwarded_headers() {
        assert!(via_trusted_proxy(&headers(&[(
            "x-forwarded-proto",
            "https"
        )])));
        assert!(via_trusted_proxy(&headers(&[(
            "x-forwarded-for",
            "1.2.3.4"
        )])));
        assert!(via_trusted_proxy(&headers(&[("x-forwarded-host", "h")])));
        assert!(!via_trusted_proxy(&headers(&[("host", "h")])));
    }

    #[test]
    fn proxied_same_host_origin_requires_forwarded_header() {
        // Behind Caddy: Origin == Host AND X-Forwarded-* present → accepted, even
        // for a vhost that isn't the configured base_url.
        assert!(origin_ok(
            &headers(&[
                ("origin", "https://weaver.rjp.io"),
                ("host", "weaver.rjp.io"),
                ("x-forwarded-proto", "https"),
            ]),
            "0.0.0.0:7878",
            None,
        ));
        // DNS-rebinding / CSWSH: a page rebinds a hostname to a directly bound
        // daemon, so Origin == Host — but a browser handshake can't add a
        // forwarded header, so it's rejected. This is the hole the gate closes.
        assert!(!origin_ok(
            &headers(&[
                ("origin", "http://evil.example:7878"),
                ("host", "evil.example:7878"),
            ]),
            "0.0.0.0:7878",
            None,
        ));
    }

    #[test]
    fn base_url_origin_trusted_without_forwarded_header() {
        let base = Some("https://loom.rjp.io");
        // The configured canonical origin is trusted however it arrived.
        assert!(origin_ok(
            &headers(&[("origin", "https://loom.rjp.io"), ("host", "loom.rjp.io")]),
            "0.0.0.0:7878",
            base,
        ));
        // A different vhost is NOT the base_url, so without proxy evidence it's
        // rejected...
        assert!(!origin_ok(
            &headers(&[
                ("origin", "https://weaver.rjp.io"),
                ("host", "weaver.rjp.io"),
            ]),
            "0.0.0.0:7878",
            base,
        ));
        // ...but Caddy supplies that evidence, so the real path still works.
        assert!(origin_ok(
            &headers(&[
                ("origin", "https://weaver.rjp.io"),
                ("host", "weaver.rjp.io"),
                ("x-forwarded-for", "1.2.3.4"),
            ]),
            "0.0.0.0:7878",
            base,
        ));
    }

    #[test]
    fn loopback_and_missing_origin_always_ok() {
        // Loopback on the bound port: no proxy headers or base_url needed.
        assert!(origin_ok(
            &headers(&[
                ("origin", "http://localhost:7878"),
                ("host", "localhost:7878"),
            ]),
            "127.0.0.1:7878",
            None,
        ));
        // A non-browser client (CLI, tests) omits Origin entirely.
        assert!(origin_ok(
            &headers(&[("host", "loom.rjp.io")]),
            "0.0.0.0:7878",
            None,
        ));
    }
}
