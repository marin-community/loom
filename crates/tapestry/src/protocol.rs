//! The control-socket wire protocol.
//!
//! Every message is a length-prefixed frame: a `u32` big-endian byte count,
//! then that many body bytes. The body's first byte is an opcode; the remainder
//! is the opcode's payload. This one framing carries both the short
//! request/response control ops and, after an [`ATTACH`](op::ATTACH), the live
//! bidirectional terminal stream.
//!
//! Keeping the framing binary (rather than JSON-per-message) matters for the
//! attach hot path: PTY output is forwarded as raw [`OUTPUT`](op::OUTPUT) frames
//! with no per-chunk encoding. The low-volume structured reply ([`PONG`](op::PONG))
//! is JSON for legibility and forward-compatibility.

use anyhow::{bail, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Opcodes. Client→server requests are the low range; server→client replies are
/// the high range; the two attach-stream opcodes (`INPUT`/`RESIZE`) are shared
/// with the terminal.rs bridge shape (`0x00`/`0x01`).
pub mod op {
    // Client → server, one-shot requests.
    /// Render the current screen to text. Payload: `u32` scrollback rows.
    pub const CAPTURE: u8 = 0x10;
    /// Write bytes to the PTY verbatim (the `send-keys -l` analogue). Payload: raw bytes.
    pub const SEND: u8 = 0x11;
    /// Resize the PTY + emulator. Payload: `u16` cols, `u16` rows.
    pub const RESIZE: u8 = 0x12;
    /// Kill the child and shut the supervisor down. No payload.
    pub const KILL: u8 = 0x13;
    /// Liveness + info probe. No payload. Answered with [`PONG`].
    pub const PING: u8 = 0x14;
    /// Switch this connection to the live terminal stream. Payload: `u16` cols,
    /// `u16` rows (the attaching client's initial size).
    pub const ATTACH: u8 = 0x20;

    // During an attach, client → server.
    /// Keystrokes to forward to the PTY. Payload: raw bytes.
    pub const INPUT: u8 = 0x00;
    // RESIZE (0x12) is reused for in-stream resizes.

    // Server → client.
    /// Reply to [`CAPTURE`]. Payload: UTF-8 screen text.
    pub const CAPTURE_RESP: u8 = 0x80;
    /// Generic success for [`SEND`]/[`RESIZE`]/[`KILL`]. No payload.
    pub const OK: u8 = 0x81;
    /// Reply to [`PING`]. Payload: JSON [`PongInfo`].
    pub const PONG: u8 = 0x82;
    /// An error reply. Payload: UTF-8 message.
    pub const ERR: u8 = 0x83;
    /// A chunk of PTY output during an attach. Payload: raw bytes.
    pub const OUTPUT: u8 = 0x90;
}

/// Reply to a [`PING`](op::PING): whether the child is alive plus a little info.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PongInfo {
    pub alive: bool,
    pub pid: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub alternate_screen: bool,
}

/// Cap on a single inbound frame body (16 MiB). Bounds a hostile or buggy peer's
/// allocation; real frames (keystrokes, a screen capture, a paste) sit far below
/// this.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// A decoded frame: its opcode and body payload (the bytes after the opcode).
#[derive(Debug, Clone)]
pub struct Frame {
    pub op: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(op: u8, payload: Vec<u8>) -> Self {
        Self { op, payload }
    }

    /// A frame whose payload is a single `u16` cols + `u16` rows (resize/attach).
    pub fn size(op: u8, cols: u16, rows: u16) -> Self {
        let mut p = Vec::with_capacity(4);
        p.extend_from_slice(&cols.to_be_bytes());
        p.extend_from_slice(&rows.to_be_bytes());
        Self::new(op, p)
    }

    /// Decode a `(cols, rows)` size payload, or `None` if it is the wrong length.
    pub fn as_size(&self) -> Option<(u16, u16)> {
        if self.payload.len() != 4 {
            return None;
        }
        let cols = u16::from_be_bytes([self.payload[0], self.payload[1]]);
        let rows = u16::from_be_bytes([self.payload[2], self.payload[3]]);
        Some((cols, rows))
    }

    /// Decode a `u32` count payload (capture history), defaulting to 0.
    pub fn as_u32(&self) -> u32 {
        if self.payload.len() == 4 {
            u32::from_be_bytes([
                self.payload[0],
                self.payload[1],
                self.payload[2],
                self.payload[3],
            ])
        } else {
            0
        }
    }
}

/// Write one frame to `w`.
pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, frame: &Frame) -> Result<()> {
    let len = 1 + frame.payload.len();
    if len > MAX_FRAME {
        bail!("frame too large: {len} bytes");
    }
    w.write_all(&(len as u32).to_be_bytes()).await?;
    w.write_all(&[frame.op]).await?;
    w.write_all(&frame.payload).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame from `r`. Returns `Ok(None)` on a clean EOF at a frame
/// boundary (the peer hung up).
pub async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Option<Frame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        // A clean EOF before any byte of the next frame is a normal disconnect.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME {
        bail!("invalid frame length: {len}");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    let op = body[0];
    let payload = body[1..].to_vec();
    Ok(Some(Frame { op, payload }))
}

/// Convenience: build the small request frames a client sends.
pub mod req {
    use super::{op, Frame};

    pub fn capture(history: u32) -> Frame {
        Frame::new(op::CAPTURE, history.to_be_bytes().to_vec())
    }
    pub fn send(data: Vec<u8>) -> Frame {
        Frame::new(op::SEND, data)
    }
    pub fn resize(cols: u16, rows: u16) -> Frame {
        Frame::size(op::RESIZE, cols, rows)
    }
    pub fn kill() -> Frame {
        Frame::new(op::KILL, Vec::new())
    }
    pub fn ping() -> Frame {
        Frame::new(op::PING, Vec::new())
    }
    pub fn attach(cols: u16, rows: u16) -> Frame {
        Frame::size(op::ATTACH, cols, rows)
    }
    pub fn input(data: Vec<u8>) -> Frame {
        Frame::new(op::INPUT, data)
    }
}
