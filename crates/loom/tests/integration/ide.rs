//! The embedded-editor reverse proxy (`crate::ide`): with a stub upstream
//! standing in for code-server, a request strips the `…/ide` prefix and reaches
//! the upstream, the bare `…/ide` redirects to the slash form, and a WebSocket
//! frame round-trips through the transparent upgrade passthrough. This exercises
//! the proxy without code-server installed (CI has none).

use std::time::Duration;

use axum::extract::ws::{Message as AxumMsg, WebSocket, WebSocketUpgrade};
use axum::response::Response as AxumResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serial_test::serial;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use crate::fixtures::TestServer;

/// A stand-in for code-server: a marker HTTP route (to prove prefix-stripping)
/// and a WebSocket echo (to prove the upgrade passthrough). Returns the port it
/// bound on loopback.
async fn start_stub_upstream() -> u16 {
    async fn ws_echo(ws: WebSocketUpgrade) -> AxumResponse {
        ws.on_upgrade(|mut socket: WebSocket| async move {
            while let Some(Ok(msg)) = socket.recv().await {
                let echo = match msg {
                    AxumMsg::Text(t) => AxumMsg::Text(t),
                    AxumMsg::Binary(b) => AxumMsg::Binary(b),
                    AxumMsg::Close(_) => break,
                    _ => continue,
                };
                if socket.send(echo).await.is_err() {
                    break;
                }
            }
        })
    }

    let app = Router::new()
        .route("/marker", get(|| async { "ok-from-upstream" }))
        .route("/ws", get(ws_echo));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ide_proxy_strips_prefix_redirects_and_passes_websockets() {
    let ts = TestServer::start().await;
    let upstream = start_stub_upstream().await;
    // Register the stub as the running editor for a session id, bypassing spawn.
    ts.ide.insert_running("stub", upstream);

    // A no-follow client so the redirect assertion sees the 308, not its target.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("http://{}", ts.addr);

    // 1. Prefix strip: the upstream sees `/marker`, not `/api/sessions/stub/ide/marker`.
    let resp = http
        .get(format!("{base}/api/sessions/stub/ide/marker"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The ETag middleware must leave proxied responses untouched: buffering
    // code-server's stream to hash it would truncate assets past the 16 MB cap.
    // A loom ETag here would mean the proxy path slipped through the exclusion.
    let etag = resp.headers().get(reqwest::header::ETAG).cloned();
    assert_eq!(resp.text().await.unwrap(), "ok-from-upstream");
    assert!(
        etag.as_ref().map(|v| v.as_bytes().starts_with(b"\"loom-")) != Some(true),
        "IDE proxy response must bypass the ETag middleware (got {etag:?})"
    );

    // 2. The bare `…/ide` (no trailing slash) 308-redirects to the slash form,
    //    preserving the query — code-server needs the trailing slash.
    let resp = http
        .get(format!("{base}/api/sessions/stub/ide?folder=/w"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 308);
    assert_eq!(
        resp.headers().get(reqwest::header::LOCATION).unwrap(),
        "/api/sessions/stub/ide/?folder=/w"
    );

    // 3. A WebSocket frame round-trips through the upgrade passthrough.
    let (mut ws, _resp) =
        tokio_tungstenite::connect_async(format!("ws://{}/api/sessions/stub/ide/ws", ts.addr))
            .await
            .expect("ws upgrade through the proxy");
    ws.send(Message::Text("ping".into())).await.unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("ws echo within 5s")
        .expect("a frame")
        .expect("a valid frame");
    assert_eq!(echoed.into_text().unwrap().as_str(), "ping");
}
