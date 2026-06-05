//! WebSocket ⇄ PTY bridge: the browser's xterm.js talks to a server-owned PTY
//! running `tmux attach`, giving a real terminal in the browser (colour, cursor,
//! mouse, scrollback, full-screen TUIs) instead of a polled read-only mirror.
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

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use tokio::sync::mpsc;

use crate::tmux;
use crate::web::{require_session, AppState};

/// Cap on a single inbound WebSocket frame. Keystrokes and resizes are tiny and
/// even a large paste is bounded; this stops a hostile client forcing a huge
/// allocation before we inspect the opcode.
const MAX_FRAME: usize = 64 * 1024;
/// PTY read-chunk size. The output channel bound is in frames, so worst-case
/// buffered output ≈ `CHANNEL_BOUND * READ_BUF`.
const READ_BUF: usize = 32 * 1024;
/// Bounded channel depth, both directions. Bounded so a slow browser
/// back-pressures the PTY (which back-pressures `tmux attach`) rather than
/// buffering without limit.
const CHANNEL_BOUND: usize = 64;

const OP_INPUT: u8 = 0x00;
const OP_RESIZE: u8 = 0x01;

/// `GET /api/sessions/{id}/terminal` — upgrade to a WebSocket bridged to the
/// session's tmux via a PTY.
pub async fn terminal_ws(
    ws: WebSocketUpgrade,
    State(st): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    // CSWSH defence: CORS does NOT apply to WebSockets, so a localhost bind is
    // not protection — validate the Origin before upgrading. See `origin_ok`.
    if !origin_ok(&headers, &st.addr) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    let session = match require_session(&st.db, &key).await {
        Ok((s, _)) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "no such session").into_response(),
    };
    // No live tmux means nothing to attach to — the caller should adopt first.
    if !tmux::has_session(&session.tmux_session).await {
        return (
            StatusCode::CONFLICT,
            "session has no running tmux — adopt it first",
        )
            .into_response();
    }
    let target = session.tmux_session.clone();
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            if let Err(e) = bridge(socket, target).await {
                tracing::debug!(error = %e, "terminal bridge ended with error");
            }
        })
}

/// Whether to allow the WebSocket upgrade.
///
/// A *missing* Origin (non-browser clients: the CLI, tests, `tokio-tungstenite`)
/// is allowed — browsers always send Origin on a WS handshake, so this does not
/// weaken the browser-CSWSH defence (it only waives same-host non-browser
/// processes, acceptable under the single-user-localhost assumption).
///
/// A *present* Origin must be a loopback host on our actual bound port. We
/// deliberately do NOT compare against the request `Host` header: Host is
/// client-supplied and a DNS-rebinding attacker can make `Origin == Host` even
/// on the default `127.0.0.1` bind. Pinning to the loopback set + the real bound
/// port closes that vector.
fn origin_ok(headers: &HeaderMap, bound_addr: &str) -> bool {
    match headers.get(axum::http::header::ORIGIN) {
        None => true,
        Some(v) => match v.to_str() {
            Ok(origin) => origin_is_loopback(origin, bound_addr),
            Err(_) => false,
        },
    }
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
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:port
        match after.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':')),
            None => return false,
        }
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        (h, Some(p))
    } else {
        (authority, None)
    };
    let port = port.unwrap_or(if scheme == "https" { "443" } else { "80" });
    if port != want_port {
        return false;
    }
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// The PTY + child + I/O handles produced by attaching to a tmux session.
struct Attach {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

/// Open a PTY and spawn `tmux attach` into it. Starts at 80×24; the client sends
/// a real size immediately after its first fit.
fn open_attach(target: &str) -> anyhow::Result<Attach> {
    let pty_system = native_pty_system();
    let PtyPair { master, slave } = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let exact = tmux::exact(target);
    let mut cmd = CommandBuilder::new("tmux");
    // `-L <socket>` (when WEAVER_TMUX_SOCKET is set) must precede the command, so
    // the attach lands on the same server `tmux::new_session` created it on.
    for flag in tmux::socket_args() {
        cmd.arg(flag);
    }
    cmd.args(["attach-session", "-t", exact.as_str()]);
    // Inherit env (same uid → same tmux socket) but drop $TMUX so attach works
    // even when loom itself runs inside tmux, and force a known TERM.
    for (k, v) in std::env::vars() {
        if k == "TMUX" {
            continue;
        }
        cmd.env(k, v);
    }
    cmd.env("TERM", "xterm-256color");

    let child = slave.spawn_command(cmd)?;
    // Drop our slave handle so that when the child exits and closes its inherited
    // slave fds, the master reader observes EIO → EOF.
    drop(slave);

    let reader = master.try_clone_reader()?;
    let writer = master.take_writer()?;
    // `clone_killer` lets us signal the child from the async side while `wait()`
    // blocks a dedicated thread.
    let killer = child.clone_killer();
    Ok(Attach {
        child,
        killer,
        reader,
        writer,
        master,
    })
}

/// Pump bytes between the WebSocket and a freshly-attached PTY until either side
/// closes, then tear down — detaching only the `tmux attach` client, never the
/// session (so the agent keeps running detached and a refresh reconnects).
async fn bridge(mut socket: WebSocket, target: String) -> anyhow::Result<()> {
    let Attach {
        mut child,
        mut killer,
        mut reader,
        mut writer,
        master,
    } = match open_attach(&target) {
        Ok(a) => a,
        Err(e) => {
            // Tell the browser why (it shows a message / backs off) rather than
            // dropping into an opaque 1006 that triggers a hot reconnect loop.
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: Utf8Bytes::from(format!("terminal attach failed: {e}")),
                })))
                .await;
            return Err(e);
        }
    };

    // The slave pty path (e.g. `/dev/pts/5`) is this attach client's controlling
    // tty as tmux sees it — captured now so teardown can detach exactly this
    // client by tty.
    let client_tty = master
        .tty_name()
        .and_then(|p| p.to_str().map(str::to_owned));

    // Keep `master` owned by *this* scope, not the input pump, so it outlives the
    // tmux client. This matters on disconnect: if the pty master is torn down
    // while `tmux attach` is still attached, tmux reacts to its terminal going
    // away by forwarding a stray Ctrl-D (EOF) to the pane — which a shell at its
    // prompt reads as end-of-input and exits on, destroying the whole session on
    // nothing more than a browser tab closing. So we detach the client first
    // (while the pty is intact) and only drop the master once the client is gone
    // (see teardown). The input pump just borrows the master (behind a mutex) to
    // apply resizes.
    let master = Arc::new(Mutex::new(master));
    let master_for_resize = Arc::clone(&master);

    // PTY output → ws: blocking reader thread → bounded mpsc → async sink.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_BOUND);
    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0u8; READ_BUF];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: child exited / slave closed
                Ok(n) => {
                    // `blocking_send` back-pressures the PTY when the browser is
                    // slow. It MUST stay on this dedicated thread — it panics if
                    // called from within the tokio runtime.
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // async consumer gone
                    }
                }
                Err(_) => break,
            }
        }
    });

    // ws input → PTY: async stream → bounded mpsc → blocking writer thread.
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_BOUND);
    let writer_thread = std::thread::spawn(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    let (mut sink, mut stream) = socket.split();

    // Output pump.
    let mut out_task = tokio::spawn(async move {
        while let Some(chunk) = out_rx.recv().await {
            if sink.send(Message::Binary(chunk.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // Input pump: decode each frame into the bytes to forward to the PTY (a
    // resize is applied inline and forwards nothing), then send once.
    //
    // When the socket closes, this is also where we *first* learn the viewer is
    // gone — and it is the right place to detach the tmux client, BEFORE any of
    // the pty fds are torn down. Detaching here (server-driven, while the client
    // terminal is still fully intact) makes `tmux attach` exit cleanly. If we
    // instead waited for the teardown below, the input pump would already have
    // ended — dropping `in_tx`/`master_for_resize` and disturbing the client
    // terminal — and tmux can react to that disturbance by forwarding a stray
    // Ctrl-D (EOF) to the pane, which exits the shell and kills the session.
    let in_task_tty = client_tty.clone();
    let mut in_task = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            let input: Vec<u8> = match msg {
                Ok(Message::Binary(payload)) => {
                    if payload.is_empty() {
                        continue;
                    }
                    match payload[0] {
                        // Forward raw bytes; never interpret as UTF-8.
                        OP_INPUT => payload[1..].to_vec(),
                        OP_RESIZE if payload.len() == 5 => {
                            let cols = u16::from_be_bytes([payload[1], payload[2]]).clamp(1, 1000);
                            let rows = u16::from_be_bytes([payload[3], payload[4]]).clamp(1, 1000);
                            if let Ok(m) = master_for_resize.lock() {
                                let _ = m.resize(PtySize {
                                    rows,
                                    cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                            }
                            continue;
                        }
                        _ => continue, // malformed resize / unknown opcode: drop
                    }
                }
                // Tolerate text frames as raw keystrokes (no opcode prefix).
                Ok(Message::Text(t)) => t.as_str().as_bytes().to_vec(),
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue, // Ping/Pong are handled by axum
            };
            if in_tx.send(input).await.is_err() {
                break;
            }
        }
        // The viewer is gone. Detach our tmux client NOW, while every pty fd is
        // still open and the client terminal is undisturbed, so `tmux attach`
        // exits cleanly without forwarding a stray Ctrl-D to the pane. Only then
        // does this task return, dropping `in_tx` (which ends the writer thread)
        // and `master_for_resize` (the bridge keeps the master alive regardless).
        detach_attached_client(in_task_tty.as_deref()).await;
    });

    // Whichever pump finishes first, abort the other.
    tokio::select! {
        _ = &mut out_task => in_task.abort(),
        _ = &mut in_task => out_task.abort(),
    }

    // Teardown. End ONLY this attach client, never the session. The input pump
    // already issued the clean, server-driven `detach-client` (above) while the
    // pty was intact; this is the ordered finish:
    //
    //   1. Backstop detach: if the *output* pump finished first, the input pump
    //      was aborted before it could detach — so detach here. A no-op if the
    //      pump already did it.
    //   2. Wait, bounded, for `tmux attach` to exit. Escalate SIGHUP then SIGKILL
    //      only for a wedged client that ignores the detach.
    //   3. Drop the master — safe now that the client is gone (no one is attached
    //      to receive the pty-close EOF).
    //   4. Reap on a blocking task so no tokio worker is blocked; join both I/O
    //      threads so nothing outlives this connection.
    detach_attached_client(client_tty.as_deref()).await;
    if !wait_for_exit(&mut child, std::time::Duration::from_millis(750)).await {
        let _ = killer.kill(); // SIGHUP
        if !wait_for_exit(&mut child, std::time::Duration::from_millis(250)).await {
            if let Some(pid) = child.process_id() {
                // SAFETY: just a signal to a pid we own; ESRCH if already reaped.
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
            wait_for_exit(&mut child, std::time::Duration::from_millis(250)).await;
        }
    }
    drop(master);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = child.wait();
    })
    .await;
    let _ = tokio::task::spawn_blocking(move || {
        let _ = reader_thread.join();
        let _ = writer_thread.join();
    })
    .await;
    Ok(())
}

/// Detach the single `tmux attach` client whose controlling tty is `tty`
/// (`None` is a no-op). Server-driven detach is the clean way to end the
/// browser bridge: the client process exits on its own without its terminal
/// teardown forwarding a stray Ctrl-D to the pane.
async fn detach_attached_client(tty: Option<&str>) {
    if let Some(tty) = tty {
        let _ = tmux::detach_client(tty).await;
    }
}

/// Poll `child` for exit up to `timeout` without blocking a tokio worker
/// (`try_wait` is non-blocking). Returns whether it exited in time.
async fn wait_for_exit(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    timeout: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::origin_is_loopback;

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
}
