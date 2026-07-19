//! Talk to a session's supervisor over its control socket.
//!
//! [`Client`] is the programmatic surface loom drives — the
//! `has_session`/`capture`/`send`/`kill` operations behind `loom::backend`. The
//! interactive attach (the xterm bridge) uses [`Client::attach`], which splits
//! the connection into an output stream + an input sink.

use anyhow::{bail, Context, Result};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::protocol::{self, op, req, Frame, PongInfo};

/// Buffered output frames an attach holds before the consumer must catch up.
/// Bounded so a slow reader back-pressures the socket (and thus the supervisor)
/// rather than buffering without limit on the client side.
const ATTACH_BUFFER: usize = 256;

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

    /// Whether a supervisor is alive for `name` — connect and ping. A stale
    /// socket file (no listener) or a supervisor reporting a dead child both read
    /// as not-alive.
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

    /// (Relay) Append raw bytes to the child's stdin — a one-shot `WRITE` outside
    /// a subscribe (the streaming equivalent is [`RelayStream::write`]). The
    /// caller sends complete newline-terminated frames; the relay writes bytes
    /// through untouched.
    pub async fn relay_write(&mut self, bytes: &[u8]) -> Result<()> {
        protocol::write_frame(&mut self.stream, &req::write(bytes.to_vec())).await?;
        self.expect_ok().await
    }

    /// (Relay) Advance the retention watermark — everything up to and including
    /// `seq` has been durably processed. A one-shot `ACK` outside a subscribe (the
    /// streaming equivalent is [`RelayStream::ack`]).
    pub async fn relay_ack(&mut self, seq: u64) -> Result<()> {
        protocol::write_frame(&mut self.stream, &req::ack(seq)).await?;
        self.expect_ok().await
    }

    /// (Relay) Switch this connection into a subscribe: the returned
    /// [`RelayStream`] first replays every spooled frame with `seq > cursor`, then
    /// streams live frames, and finally an [`RelayEvent::Exit`] once the child has
    /// exited. Only one subscriber exists at a time — this evicts any previous one.
    pub async fn subscribe(self, cursor: u64) -> Result<RelayStream> {
        let mut stream = self.stream;
        protocol::write_frame(&mut stream, &req::subscribe(cursor)).await?;
        let (rd, wr) = stream.into_split();

        // A background reader decodes FRAME/EXIT frames into a bounded channel;
        // `send().await` back-pressures the socket (and thus the supervisor) when
        // the consumer is slow, mirroring the attach path.
        let (ev_tx, ev_rx) = mpsc::channel::<RelayEvent>(ATTACH_BUFFER);
        tokio::spawn(async move {
            let mut rd = rd;
            loop {
                match protocol::read_frame(&mut rd).await {
                    Ok(Some(frame)) if frame.op == op::FRAME => {
                        let Some((seq, body)) = frame.as_relay_frame() else {
                            continue;
                        };
                        let ev = RelayEvent::Frame {
                            seq,
                            payload: body.to_vec(),
                        };
                        if ev_tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                    Ok(Some(frame)) if frame.op == op::EXIT => {
                        let status =
                            serde_json::from_slice::<Option<i32>>(&frame.payload).unwrap_or(None);
                        let _ = ev_tx.send(RelayEvent::Exit { status }).await;
                        // EXIT is terminal for the stream; nothing follows it.
                        break;
                    }
                    Ok(Some(_)) => {} // ignore stray frames
                    Ok(None) | Err(_) => break,
                }
            }
        });

        Ok(RelayStream { wr, ev_rx })
    }

    /// Switch this connection into an interactive attach at the given size. The
    /// returned [`Attach`] streams PTY output and accepts input/resize; the
    /// first output chunk is a full repaint of the current screen.
    pub async fn attach(self, cols: u16, rows: u16) -> Result<Attach> {
        let mut stream = self.stream;
        protocol::write_frame(&mut stream, &req::attach(cols, rows)).await?;
        let (rd, wr) = stream.into_split();

        // Output: a background reader task drains OUTPUT frames into a bounded
        // channel. The `send().await` is the back-pressure hinge — when the
        // consumer is slow the channel fills, this task stops reading the socket,
        // and that propagates back to the supervisor (whose own bounded
        // per-subscriber channel then fills and evicts only a genuinely wedged
        // client).
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(ATTACH_BUFFER);
        tokio::spawn(async move {
            let mut rd = rd;
            loop {
                match protocol::read_frame(&mut rd).await {
                    Ok(Some(frame)) if frame.op == op::OUTPUT => {
                        if out_tx.send(frame.payload).await.is_err() {
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
    out_rx: mpsc::Receiver<Vec<u8>>,
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

    /// Split into independent input and output halves, so a bridge can pump each
    /// direction in its own task (the WebSocket terminal needs this; the CLI
    /// drives the combined struct in a `select!` instead).
    pub fn split(self) -> (AttachInput, AttachOutput) {
        (
            AttachInput { wr: self.wr },
            AttachOutput {
                out_rx: self.out_rx,
            },
        )
    }
}

/// The input half of a split [`Attach`]: forward keystrokes and resizes.
pub struct AttachInput {
    wr: OwnedWriteHalf,
}

impl AttachInput {
    /// Forward keystrokes to the PTY.
    pub async fn send_input(&mut self, data: &[u8]) -> Result<()> {
        protocol::write_frame(&mut self.wr, &req::input(data.to_vec())).await
    }

    /// Resize the PTY + emulator.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        protocol::write_frame(&mut self.wr, &req::resize(cols, rows)).await
    }
}

/// The output half of a split [`Attach`]: a stream of PTY output chunks.
pub struct AttachOutput {
    out_rx: mpsc::Receiver<Vec<u8>>,
}

impl AttachOutput {
    /// The next chunk of PTY output, or `None` once the stream ends.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.out_rx.recv().await
    }
}

/// One event from a relay [`RelayStream`]: a relayed child-stdout frame, or the
/// child's exit. `Exit` is terminal — no event follows it.
#[derive(Debug, Clone)]
pub enum RelayEvent {
    /// A relayed child-stdout frame with its spool sequence number.
    Frame { seq: u64, payload: Vec<u8> },
    /// The child exited. `status` is its code (a signal death is `128 + signal`),
    /// or `None` if unknown.
    Exit { status: Option<i32> },
}

/// A live relay subscription: receive [`RelayEvent`]s (replayed then live frames,
/// then a terminal `Exit`), and send `ACK`/`WRITE` back to the supervisor.
/// Dropping it closes the connection, which the supervisor sees as an
/// unsubscribe (the child keeps running).
pub struct RelayStream {
    wr: OwnedWriteHalf,
    ev_rx: mpsc::Receiver<RelayEvent>,
}

impl RelayStream {
    /// The next relay event, or `None` once the stream ends (the socket closed).
    pub async fn recv(&mut self) -> Option<RelayEvent> {
        self.ev_rx.recv().await
    }

    /// Advance the retention watermark: everything up to and including `seq` has
    /// been durably processed, so the supervisor may drop fully-acked spool
    /// segments.
    pub async fn ack(&mut self, seq: u64) -> Result<()> {
        protocol::write_frame(&mut self.wr, &req::ack(seq)).await
    }

    /// Append raw bytes to the child's stdin (send complete newline-terminated
    /// frames; the relay writes them through untouched).
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        protocol::write_frame(&mut self.wr, &req::write(bytes.to_vec())).await
    }
}
