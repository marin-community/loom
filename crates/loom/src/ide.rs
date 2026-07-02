//! Embedded VS Code (code-server) per session, reverse-proxied through loom.
//!
//! The terminal ([`crate::terminal`]) bridges a PTY; this bridges a whole
//! code-server HTTP+WebSocket server. One code-server per session, spawned
//! lazily and rooted at the session's worktree, bound to an ephemeral loopback
//! port with `--auth none`. It is reachable only through loom's authenticated
//! reverse proxy (`/api/sessions/{id}/ide/*`), so it needs no password of its
//! own and the iframe rides loom's session cookie.
//!
//! See `docs/embedded-ide.md` for the design. The code-server is a **child of
//! loom** (not detached like tapestry): it holds no irreplaceable state — the
//! files are on disk — so a loom restart drops it and the next access respawns
//! it. That keeps the manager a plain in-memory map with no DB table.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::extract::{OriginalUri, State};
use axum::http::header::{
    CONNECTION, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING,
    UPGRADE,
};
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri};
use axum::response::{IntoResponse, Json, Redirect, Response};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, OnceCell};

use crate::db::Db;
use crate::web::{require_session, AppState};
use weaver_core::config;

/// How often the reaper wakes to retire idle editors.
const REAP_TICK: Duration = Duration::from_secs(60);
/// How long to wait for a freshly-spawned code-server to report its port.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(90);
/// Fallback idle-reap timeout when the `ide.idle_timeout_secs` setting is unset
/// or unparseable. Mirrors that setting's registry default.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 1800;

/// One running (or externally-registered) code-server.
struct Instance {
    port: u16,
    /// Unix seconds of the last proxied request — the reaper's idle clock.
    last_access: AtomicI64,
    /// The child process, or `None` for a test-/externally-managed upstream.
    /// `kill_on_drop` is set, so dropping this instance also kills the child.
    child: Mutex<Option<Child>>,
    /// Per-instance state dir under `$WEAVER_HOME/ide/<id>`, removed on kill.
    state_dir: Option<PathBuf>,
}

impl Instance {
    fn touch(&self) {
        self.last_access.store(now_unix(), Ordering::Relaxed);
    }

    fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
        if let Some(dir) = &self.state_dir {
            let dir = dir.clone();
            tokio::spawn(async move {
                let _ = tokio::fs::remove_dir_all(dir).await;
            });
        }
    }
}

/// Per-session code-server lifecycle + the reverse-proxy upstream registry.
/// Held in [`AppState`] behind an `Arc`.
pub struct IdeManager {
    inner: Mutex<HashMap<String, Arc<OnceCell<Arc<Instance>>>>>,
    /// `$WEAVER_HOME/ide` — per-instance user-data/extensions dirs live under it.
    home: PathBuf,
    /// Pooled client for the plain-HTTP plane (assets, the editor's own API).
    client: Client<HttpConnector, Body>,
    /// Cached `code-server --version` probe, so a host without it degrades to a
    /// clear message instead of a broken iframe.
    available: OnceCell<bool>,
}

/// `$WEAVER_HOME/ide` — the root under which per-session editor state lives.
pub fn ide_home() -> PathBuf {
    weaver_core::db::weaver_home().join("ide")
}

impl IdeManager {
    pub fn new(home: PathBuf) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            home,
            client: Client::builder(TokioExecutor::new()).build_http(),
            available: OnceCell::new(),
        }
    }

    /// Resolve (spawning if needed) the loopback port serving `id`'s editor.
    /// On the hot path (an already-running instance) this is a lock + map read,
    /// no DB. A cold start resolves the worktree via [`require_session`] and
    /// spawns; concurrent first-hits share the one spawn via the `OnceCell`.
    /// Errors are returned as a ready [`Response`] so the proxy can relay them.
    async fn ensure(&self, st: &AppState, id: &str) -> Result<u16, Response> {
        let cell = {
            let mut map = self.inner.lock().unwrap();
            map.entry(id.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        if let Some(inst) = cell.get() {
            inst.touch();
            return Ok(inst.port);
        }
        // Cold start: resolve the worktree and the launch command, then spawn.
        let (session, _) = require_session(&st.db, id)
            .await
            .map_err(IntoResponse::into_response)?;
        let command = ide_command(&st.db).await;
        let home = self.home.join(id);
        let work_dir = session.work_dir.clone();
        let inst = cell
            .get_or_try_init(|| spawn(command, home, work_dir))
            .await
            .map_err(|msg| (StatusCode::BAD_GATEWAY, msg).into_response())?;
        inst.touch();
        Ok(inst.port)
    }

    /// Whether the `code-server` command is runnable. Probed once and cached.
    pub async fn available(&self, db: &Db) -> bool {
        *self
            .available
            .get_or_init(|| async {
                let command = ide_command(db).await;
                let mut parts = command.split_whitespace();
                let Some(program) = parts.next() else {
                    return false;
                };
                Command::new(program)
                    .args(parts)
                    .arg("--version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .map(|s| s.success())
                    .unwrap_or(false)
            })
            .await
    }

    /// Tear down a session's editor (called from archive/remove teardown).
    pub fn kill(&self, id: &str) {
        let cell = self.inner.lock().unwrap().remove(id);
        if let Some(cell) = cell {
            if let Some(inst) = cell.get() {
                tracing::info!(session = id, port = inst.port, "code-server stopped");
                inst.kill();
            }
            // A spawn still in flight has no `get()` yet; its child carries
            // `kill_on_drop`, so dropping the orphaned cell kills it.
        }
    }

    /// Retire instances idle longer than `idle`.
    fn reap(&self, idle: Duration) {
        let now = now_unix();
        let cutoff = idle.as_secs() as i64;
        let victims: Vec<String> = {
            let map = self.inner.lock().unwrap();
            map.iter()
                .filter_map(|(id, cell)| {
                    let inst = cell.get()?;
                    (now - inst.last_access.load(Ordering::Relaxed) > cutoff).then(|| id.clone())
                })
                .collect()
        };
        for id in victims {
            self.kill(&id);
        }
    }

    /// Register an already-running upstream port for a session, bypassing spawn.
    /// Used by integration tests to point the proxy at a stub server.
    pub fn insert_running(&self, id: &str, port: u16) {
        let cell = Arc::new(OnceCell::new());
        let _ = cell.set(Arc::new(Instance {
            port,
            last_access: AtomicI64::new(now_unix()),
            child: Mutex::new(None),
            state_dir: None,
        }));
        self.inner.lock().unwrap().insert(id.to_string(), cell);
    }
}

/// The reaper loop — spawned beside the monitor in `server::serve`.
pub async fn reap_loop(state: AppState) {
    loop {
        tokio::time::sleep(REAP_TICK).await;
        state
            .ide
            .reap(Duration::from_secs(idle_timeout_secs(&state.db).await));
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The configured idle-reap timeout, falling back to [`DEFAULT_IDLE_TIMEOUT_SECS`].
async fn idle_timeout_secs(db: &Db) -> u64 {
    config::get_or(db, "ide.idle_timeout_secs", "")
        .await
        .parse()
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS)
}

/// The launch command: `WEAVER_IDE_CMD`, else the `ide.command` setting, else
/// `code-server`. May carry leading args (split on whitespace) so a test can
/// point it at a stub.
async fn ide_command(db: &Db) -> String {
    if let Ok(c) = std::env::var("WEAVER_IDE_CMD") {
        if !c.trim().is_empty() {
            return c;
        }
    }
    let c = config::get_or(db, "ide.command", "").await;
    if c.trim().is_empty() {
        "code-server".to_string()
    } else {
        c
    }
}

/// Spawn a code-server rooted at `work_dir`, bound to an ephemeral loopback
/// port, and resolve once it logs the port it bound. `home` is its per-instance
/// state dir.
async fn spawn(command: String, home: PathBuf, work_dir: String) -> Result<Arc<Instance>, String> {
    let data = home.join("data");
    let ext = home.join("ext");
    let user = data.join("User");
    let _ = tokio::fs::create_dir_all(&user).await;
    let _ = tokio::fs::create_dir_all(&ext).await;
    // Land in the Explorer, not the Welcome tab.
    let settings = user.join("settings.json");
    if tokio::fs::metadata(&settings).await.is_err() {
        let _ = tokio::fs::write(&settings, br#"{"workbench.startupEditor":"none"}"#).await;
    }
    // A throwaway config so the operator's ~/.config/code-server/config.yaml
    // can't override the launch flags below.
    let cfg = home.join("config.yaml");
    let _ = tokio::fs::write(&cfg, b"").await;

    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or("code-server");
    let prefix_args: Vec<&str> = parts.collect();

    let mut cmd = Command::new(program);
    cmd.args(prefix_args)
        .arg("--bind-addr")
        .arg("127.0.0.1:0")
        .arg("--auth")
        .arg("none")
        .arg("--disable-telemetry")
        .arg("--disable-update-check")
        .arg("--disable-workspace-trust")
        .arg("--user-data-dir")
        .arg(&data)
        .arg("--extensions-dir")
        .arg(&ext)
        .arg(&work_dir)
        .env("CODE_SERVER_CONFIG", &cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not launch code-server (`{program}`): {e}"))?;

    let port = wait_for_port(&mut child).await?;
    let session = home
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    tracing::info!(session = %session, port, "code-server started");
    Ok(Arc::new(Instance {
        port,
        last_access: AtomicI64::new(now_unix()),
        child: Mutex::new(Some(child)),
        state_dir: Some(home),
    }))
}

/// Read the child's stdout+stderr until it logs `HTTP server listening on
/// http://127.0.0.1:<port>/`, then keep draining the pipes (to the trace log) so
/// a chatty code-server never blocks on a full pipe.
async fn wait_for_port(child: &mut Child) -> Result<u16, String> {
    let (tx, mut rx) = mpsc::channel::<String>(64);
    if let Some(out) = child.stdout.take() {
        pump_lines(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump_lines(err, tx);
    }
    let deadline = tokio::time::sleep(SPAWN_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Err("code-server did not report a listening port in time".into()),
            line = rx.recv() => match line {
                Some(line) => if let Some(port) = parse_listening_port(&line) {
                    return Ok(port);
                },
                None => return Err("code-server exited before reporting a listening port".into()),
            },
        }
    }
}

fn pump_lines<R: AsyncRead + Unpin + Send + 'static>(reader: R, tx: mpsc::Sender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "loom::ide", "{line}");
            // Once the port is found the receiver is dropped; the send then
            // fails fast and we keep looping to drain (not block) the pipe.
            let _ = tx.send(line).await;
        }
    });
}

/// Pull the port out of a `… HTTP server listening on http://127.0.0.1:34543/`
/// line. Ignores the unix-socket "Session server listening on …" line (no
/// `127.0.0.1:`), so only the HTTP listener matches.
fn parse_listening_port(line: &str) -> Option<u16> {
    let marker = "127.0.0.1:";
    let at = line.find("listening on")?;
    let rest = &line[at..];
    let start = rest.find(marker)? + marker.len();
    let digits: String = rest[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

// ---------------------------------------------------------------------------
// The reverse proxy
// ---------------------------------------------------------------------------

/// What to do with an incoming `…/ide…` request after stripping the prefix.
enum Target {
    /// `…/ide` (no trailing slash) → 308 to this location. code-server derives
    /// its base path from relative URLs, so the trailing slash is load-bearing.
    Redirect(String),
    /// Forward this `path?query` (always starts with `/`) to the upstream.
    Forward(String),
}

/// Parse the session id out of `/api/sessions/<id>/ide…`.
fn parse_session_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/sessions/")?;
    let id = rest.split('/').next()?;
    (!id.is_empty()).then_some(id)
}

/// Strip the `/api/sessions/<id>/ide` prefix, yielding the upstream path+query
/// (or a redirect to the slash form). code-server has no base-path flag, so the
/// proxy must mount it at `/`.
fn route_target(orig: &Uri, id: &str) -> Target {
    let prefix = format!("/api/sessions/{id}/ide");
    let path = orig.path();
    let query = orig.query();
    if path == prefix {
        let mut loc = format!("{prefix}/");
        if let Some(q) = query {
            loc.push('?');
            loc.push_str(q);
        }
        return Target::Redirect(loc);
    }
    let suffix = path.strip_prefix(&prefix).unwrap_or("/");
    let mut out = suffix.to_string();
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Target::Forward(out)
}

fn is_ws_upgrade(headers: &HeaderMap) -> bool {
    let upgrade = headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let conn = headers
        .get(CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"));
    upgrade && conn
}

/// Headers that are connection-scoped and must not be forwarded across the
/// proxy hop (except on a WebSocket upgrade, where `Connection`/`Upgrade` *are*
/// the signal and are forwarded verbatim).
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    for h in [
        CONNECTION,
        PROXY_AUTHENTICATE,
        PROXY_AUTHORIZATION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
    ] {
        headers.remove(h);
    }
    // No `KEEP_ALIVE` const in the http crate; remove it by name.
    headers.remove(HeaderName::from_static("keep-alive"));
}

/// `ANY /api/sessions/{id}/ide` and `…/ide/{*rest}` — the reverse proxy onto the
/// session's code-server. Plain HTTP is pooled through the legacy client; a
/// WebSocket upgrade is a transparent byte passthrough (see [`proxy_ws`]).
pub async fn proxy(
    State(st): State<AppState>,
    OriginalUri(orig): OriginalUri,
    req: Request,
) -> Response {
    if !config::get_bool(&st.db, "ide.enabled", true).await {
        return (StatusCode::SERVICE_UNAVAILABLE, "editor is disabled").into_response();
    }
    let Some(id) = parse_session_id(orig.path()).map(str::to_string) else {
        return (StatusCode::BAD_REQUEST, "malformed editor path").into_response();
    };
    let suffix = match route_target(&orig, &id) {
        Target::Redirect(loc) => return Redirect::permanent(&loc).into_response(),
        Target::Forward(suffix) => suffix,
    };
    let port = match st.ide.ensure(&st, &id).await {
        Ok(port) => port,
        Err(resp) => return resp,
    };
    if is_ws_upgrade(req.headers()) {
        proxy_ws(req, port, &suffix).await
    } else {
        proxy_http(&st.ide.client, req, port, &suffix).await
    }
}

/// Forward a plain HTTP request to the loopback code-server and stream the
/// response back. Host is dropped (the client sets it to the loopback target);
/// code-server only origin-checks WebSocket upgrades, not plain HTTP.
async fn proxy_http(
    client: &Client<HttpConnector, Body>,
    req: Request,
    port: u16,
    suffix: &str,
) -> Response {
    let (mut parts, body) = req.into_parts();
    let Ok(uri) = format!("http://127.0.0.1:{port}{suffix}").parse::<Uri>() else {
        return (StatusCode::BAD_GATEWAY, "bad upstream uri").into_response();
    };
    parts.uri = uri;
    strip_hop_by_hop(&mut parts.headers);
    parts.headers.remove(HOST);
    let upstream = Request::from_parts(parts, body);
    match client.request(upstream).await {
        Ok(resp) => {
            let (mut parts, incoming) = resp.into_parts();
            strip_hop_by_hop(&mut parts.headers);
            Response::from_parts(parts, Body::new(incoming))
        }
        Err(e) => {
            tracing::debug!(target: "loom::ide", "http proxy error: {e}");
            (StatusCode::BAD_GATEWAY, "editor backend unreachable").into_response()
        }
    }
}

/// Transparent WebSocket-upgrade passthrough. code-server runs a same-origin
/// check on every upgrade (comparing `Origin` to `Host`), so headers — `Host`
/// and `Origin` included — are forwarded verbatim. After both ends switch
/// protocols, bytes are copied bidirectionally until either closes.
async fn proxy_ws(mut req: Request, port: u16, suffix: &str) -> Response {
    // Claim the client-side upgrade future before consuming the request; it
    // resolves once we return the 101 below.
    let client_on = hyper::upgrade::on(&mut req);
    let (parts, _body) = req.into_parts();

    let stream = match TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_GATEWAY, "editor backend unreachable").into_response(),
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(pair) => pair,
        Err(_) => return (StatusCode::BAD_GATEWAY, "editor handshake failed").into_response(),
    };
    // Drive the upstream connection (with upgrade support) in the background.
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });

    let Ok(uri) = format!("http://127.0.0.1:{port}{suffix}").parse::<Uri>() else {
        return (StatusCode::BAD_GATEWAY, "bad upstream uri").into_response();
    };
    let mut builder = Request::builder().method(parts.method).uri(uri);
    for (name, value) in parts.headers.iter() {
        builder = builder.header(name, value);
    }
    let Ok(upstream_req) = builder.body(Body::empty()) else {
        return (StatusCode::BAD_GATEWAY, "bad upstream request").into_response();
    };

    let mut upstream_resp = match sender.send_request(upstream_req).await {
        Ok(resp) => resp,
        Err(_) => return (StatusCode::BAD_GATEWAY, "editor backend unreachable").into_response(),
    };
    if upstream_resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        // Not actually an upgrade (e.g. a 404/403) — relay it through.
        let (parts, incoming) = upstream_resp.into_parts();
        return Response::from_parts(parts, Body::new(incoming));
    }

    let upstream_on = hyper::upgrade::on(&mut upstream_resp);
    tokio::spawn(async move {
        if let (Ok(client_io), Ok(upstream_io)) = tokio::join!(client_on, upstream_on) {
            let mut client = TokioIo::new(client_io);
            let mut upstream = TokioIo::new(upstream_io);
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
    });

    // Return the upstream's 101 verbatim (Sec-WebSocket-Accept, Upgrade,
    // Connection) so the browser completes the same handshake code-server made.
    let mut response = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    for (name, value) in upstream_resp.headers().iter() {
        response = response.header(name, value);
    }
    response
        .body(Body::empty())
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "").into_response())
}

// ---------------------------------------------------------------------------
// Status endpoint
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct IdeInfo {
    /// The `ide.enabled` master switch.
    enabled: bool,
    /// Whether the `code-server` command is runnable on this host.
    available: bool,
    /// Idle-reap timeout, surfaced for the UI's info text.
    idle_timeout_secs: i64,
}

/// `GET /api/sessions/{id}/ide-info` — lets the UI decide whether to mount the
/// iframe or show a "code-server not installed" note. Never spawns.
pub async fn info(State(st): State<AppState>) -> Json<IdeInfo> {
    let enabled = config::get_bool(&st.db, "ide.enabled", true).await;
    let available = enabled && st.ide.available(&st.db).await;
    Json(IdeInfo {
        enabled,
        available,
        idle_timeout_secs: idle_timeout_secs(&st.db).await as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_http_listener_port() {
        let line =
            "[2026-06-16T06:18:41.170Z] info  HTTP server listening on http://127.0.0.1:34543/";
        assert_eq!(parse_listening_port(line), Some(34543));
    }

    #[test]
    fn ignores_the_session_socket_line() {
        // The unix-socket line has no 127.0.0.1: authority, so it must not match.
        let line = "[t] info  Session server listening on /run/user/1000/code-server/…";
        assert_eq!(parse_listening_port(line), None);
        assert_eq!(parse_listening_port("nothing here"), None);
    }

    #[test]
    fn parses_session_id_from_path() {
        assert_eq!(
            parse_session_id("/api/sessions/abc123/ide/static/x.js"),
            Some("abc123")
        );
        assert_eq!(parse_session_id("/api/sessions/abc123/ide"), Some("abc123"));
        assert_eq!(parse_session_id("/api/sessions//ide"), None);
        assert_eq!(parse_session_id("/api/health"), None);
    }

    #[test]
    fn bare_ide_redirects_to_trailing_slash() {
        let uri: Uri = "/api/sessions/s1/ide".parse().unwrap();
        match route_target(&uri, "s1") {
            Target::Redirect(loc) => assert_eq!(loc, "/api/sessions/s1/ide/"),
            Target::Forward(_) => panic!("expected redirect"),
        }
    }

    #[test]
    fn bare_ide_redirect_preserves_query() {
        let uri: Uri = "/api/sessions/s1/ide?folder=/w".parse().unwrap();
        match route_target(&uri, "s1") {
            Target::Redirect(loc) => assert_eq!(loc, "/api/sessions/s1/ide/?folder=/w"),
            Target::Forward(_) => panic!("expected redirect"),
        }
    }

    #[test]
    fn strips_prefix_to_upstream_path() {
        let uri: Uri = "/api/sessions/s1/ide/static/out/main.js?v=2"
            .parse()
            .unwrap();
        match route_target(&uri, "s1") {
            Target::Forward(suffix) => assert_eq!(suffix, "/static/out/main.js?v=2"),
            Target::Redirect(_) => panic!("expected forward"),
        }
    }

    #[test]
    fn trailing_slash_root_forwards_slash() {
        let uri: Uri = "/api/sessions/s1/ide/?folder=/w".parse().unwrap();
        match route_target(&uri, "s1") {
            Target::Forward(suffix) => assert_eq!(suffix, "/?folder=/w"),
            Target::Redirect(_) => panic!("expected forward"),
        }
    }

    #[test]
    fn detects_websocket_upgrade() {
        let mut h = HeaderMap::new();
        assert!(!is_ws_upgrade(&h));
        h.insert(UPGRADE, "websocket".parse().unwrap());
        h.insert(CONNECTION, "Upgrade".parse().unwrap());
        assert!(is_ws_upgrade(&h));
    }
}
