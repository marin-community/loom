//! Talk to a session's supervisor over its control socket.
//!
//! [`Client`] is the programmatic surface loom drives — the tapestry analogue of
//! the `tmux::{has_session, capture, send_*, kill_session}` calls. The
//! interactive attach (the xterm bridge) uses [`Client::attach`], which splits
//! the connection into an output stream + an input sink.

use anyhow::{bail, Context, Result};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::protocol::{self, op, req, Frame, PongInfo};

/// A connected control client for one session.
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connect to a session by name. Errors if no supervisor is listening (the
    /// `has_session` == false case).
    pub async fn connect(name: &str) -> Result<Self> {
        let path = crate::paths::socket_path(name);
        let stream = UnixStream::connect(&path)
            .await
            .with_context(|| format!("connecting to session {name}"))?;
        Ok(Self { stream })
    }

    /// Whether a supervisor is alive for `name` — connect and ping. The
    /// `tmux has-session` analogue: a stale socket file (no listener) or a
    /// supervisor reporting a dead child both read as not-alive.
    pub async fn is_alive(name: &str) -> bool {
        match Self::connect(name).await {
            Ok(mut c) => c.ping().await.map(|p| p.alive).unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Ping: liveness + screen info.
    pub async fn ping(&mut self) -> Result<PongInfo> {
        protocol::write_frame(&mut self.stream, &req::ping()).await?;
        let frame = self.read_reply().await?;
        if frame.op != op::PONG {
            bail!("unexpected reply to ping: {:#x}", frame.op);
        }
        let info: Option<PongInfo> = serde_json::from_slice(&frame.payload)?;
        info.context("supervisor returned no pong info")
    }

    /// Render the current screen to text; `history` extra scrollback rows.
    pub async fn capture(&mut self, history: u32) -> Result<String> {
        protocol::write_frame(&mut self.stream, &req::capture(history)).await?;
        let frame = self.read_reply().await?;
        match frame.op {
            op::CAPTURE_RESP => Ok(String::from_utf8_lossy(&frame.payload).into_owned()),
            op::ERR => bail!(
                "capture failed: {}",
                String::from_utf8_lossy(&frame.payload)
            ),
            other => bail!("unexpected reply to capture: {other:#x}"),
        }
    }

    /// Write bytes to the PTY verbatim (the `send-keys -l` analogue).
    pub async fn send(&mut self, data: &[u8]) -> Result<()> {
        protocol::write_frame(&mut self.stream, &req::send(data.to_vec())).await?;
        self.expect_ok().await
    }

    /// Resize the PTY + emulator.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        protocol::write_frame(&mut self.stream, &req::resize(cols, rows)).await?;
        self.expect_ok().await
    }

    /// Kill the child and shut the supervisor down.
    pub async fn kill(&mut self) -> Result<()> {
        protocol::write_frame(&mut self.stream, &req::kill()).await?;
        self.expect_ok().await
    }

    /// Switch this connection into an interactive attach at the given size. The
    /// returned [`Attach`] streams PTY output and accepts input/resize; the
    /// first output chunk is a full repaint of the current screen.
    pub async fn attach(self, cols: u16, rows: u16) -> Result<Attach> {
        let mut stream = self.stream;
        protocol::write_frame(&mut stream, &req::attach(cols, rows)).await?;
        let (rd, wr) = stream.into_split();

        // Output: a background reader task drains OUTPUT frames into a channel.
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            let mut rd = rd;
            loop {
                match protocol::read_frame(&mut rd).await {
                    Ok(Some(frame)) if frame.op == op::OUTPUT => {
                        if out_tx.send(frame.payload).is_err() {
                            break;
                        }
                    }
                    Ok(Some(_)) => {} // ignore non-output frames
                    Ok(None) | Err(_) => break,
                }
            }
        });

        Ok(Attach { wr, out_rx })
    }

    async fn read_reply(&mut self) -> Result<Frame> {
        protocol::read_frame(&mut self.stream)
            .await?
            .context("supervisor closed the connection")
    }

    async fn expect_ok(&mut self) -> Result<()> {
        let frame = self.read_reply().await?;
        match frame.op {
            op::OK => Ok(()),
            op::ERR => bail!("error: {}", String::from_utf8_lossy(&frame.payload)),
            other => bail!("unexpected reply: {other:#x}"),
        }
    }
}

/// A live attach: receive PTY output, send input and resizes. Dropping it closes
/// the connection, which the supervisor sees as a detach (the child keeps
/// running).
pub struct Attach {
    wr: OwnedWriteHalf,
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Attach {
    /// The next chunk of PTY output, or `None` once the stream ends.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.out_rx.recv().await
    }

    /// Forward keystrokes to the PTY.
    pub async fn send_input(&mut self, data: &[u8]) -> Result<()> {
        protocol::write_frame(&mut self.wr, &req::input(data.to_vec())).await
    }

    /// Resize the PTY + emulator.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        protocol::write_frame(&mut self.wr, &req::resize(cols, rows)).await
    }
}
