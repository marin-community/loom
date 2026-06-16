//! The session supervisor: owns one PTY, a vt100 screen emulator, and a unix
//! control socket. It is the process that *outlives loom* — start it detached
//! (see [`spawn_detached`](crate::spawn_detached)) and the agent keeps running
//! across a loom restart. It streams **raw PTY bytes** to attached clients, so an
//! xterm owns its own scrollback/selection/search.
//!
//! ## Shape
//!
//! A single async *core task* is the one owner of mutable state — the vt100
//! parser (the screen), the set of attached clients, the PTY master (for
//! resize), and the writer channel. Everything else talks to it over an mpsc of
//! [`Cmd`]:
//!
//! * a blocking **reader thread** pumps PTY output into a *bounded* channel (its
//!   `blocking_send` back-pressures the PTY — and thus the child — when viewers
//!   are slow, so a terminal stream is never silently truncated);
//! * a blocking **writer thread** drains an mpsc into the PTY;
//! * a blocking **wait thread** reaps the child → `Cmd::ChildExited`;
//! * each accepted socket connection is a task issuing control `Cmd`s and, on
//!   attach, registering an output subscriber.
//!
//! Output fan-out applies back-pressure: the core *awaits* each subscriber's
//! bounded channel, so a slow viewer slows the read side rather than dropping
//! bytes (xterm needs every byte to stay coherent). Back-pressure stops at a
//! genuinely wedged viewer: a subscriber that accepts nothing for
//! [`EVICT_AFTER`] is dropped (its client reconnects and repaints) so one dead
//! viewer can neither stall the child forever nor grow memory without bound.
//! Control commands are polled ahead of output each iteration, so liveness and
//! teardown stay responsive.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{self, op, Frame, PongInfo};

/// PTY read-chunk size — matches the terminal.rs bridge.
const READ_BUF: usize = 32 * 1024;
/// Default emulator scrollback (rows retained above the screen for `capture`
/// with history). xterm keeps its own client-side scrollback off the live
/// stream; this only bounds what a *programmatic* capture can reach back to.
const DEFAULT_SCROLLBACK: usize = 1000;
/// PTY-output backlog (chunks) buffered from the reader thread before its
/// `blocking_send` parks — the point where back-pressure reaches the child.
const OUTPUT_BOUND: usize = 256;
/// Per-subscriber output backlog before the core starts *awaiting* that viewer
/// (the back-pressure hinge). At up to `READ_BUF` per chunk this is a few MiB of
/// slack before a viewer's slowness reaches the read side.
const SUBSCRIBER_BOUND: usize = 256;
/// How long a single fan-out send may block on one viewer before that viewer is
/// judged wedged and evicted. Long enough that a merely-slow client is waited
/// for; short enough that a dead socket cannot freeze the child indefinitely.
const EVICT_AFTER: Duration = Duration::from_secs(30);

/// What to run under the supervisor.
pub struct SupervisorConfig {
    /// Session name — the socket file stem.
    pub name: String,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Shell script run as `sh -c <script>`.
    pub script: String,
    /// Extra environment for the child, on top of the inherited environment.
    pub env: Vec<(String, String)>,
    /// Initial PTY size.
    pub cols: u16,
    pub rows: u16,
}

/// Control messages to the core task. The single owner of screen + client state.
/// PTY output travels on its own bounded channel (so the reader can park under
/// back-pressure), not through here.
enum Cmd {
    /// Forward bytes to the PTY verbatim.
    Send(Vec<u8>),
    /// Resize PTY + emulator.
    Resize(u16, u16),
    /// Render the screen to text; `history` extra scrollback rows.
    Capture {
        history: u32,
        resp: oneshot::Sender<String>,
    },
    /// Liveness + info.
    Ping { resp: oneshot::Sender<PongInfo> },
    /// Register an output subscriber at the given size; replies with its id and
    /// the initial repaint bytes to send first.
    Attach {
        cols: u16,
        rows: u16,
        out_tx: mpsc::Sender<Vec<u8>>,
        resp: oneshot::Sender<u64>,
    },
    /// Drop a subscriber (its client disconnected).
    Detach(u64),
    /// Kill the child and shut down.
    Kill,
    /// The child exited on its own.
    ChildExited,
}

/// Run the supervisor to completion: bring up the PTY + child, serve the control
/// socket, and return once the child exits or a [`Kill`](Cmd::Kill) arrives. The
/// socket file is removed on the way out.
pub async fn run(cfg: SupervisorConfig) -> Result<()> {
    let socket = crate::paths::socket_path(&cfg.name);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating run dir {}", parent.display()))?;
    }
    // A stale socket from a prior crash would make bind() fail with EADDRINUSE.
    // A successful connect proves a live supervisor is already listening for this
    // name — refuse rather than clobber it; otherwise the file is stale, remove it.
    if socket.exists() {
        if tokio::net::UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!("session {} already has a live supervisor", cfg.name);
        }
        let _ = std::fs::remove_file(&socket);
    }

    // --- PTY + child -------------------------------------------------------
    let pty_system = native_pty_system();
    let PtyPair { master, slave } = pty_system
        .openpty(PtySize {
            rows: cfg.rows.max(1),
            cols: cfg.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    let mut builder = CommandBuilder::new("sh");
    builder.args(["-c", &cfg.script]);
    builder.cwd(&cfg.cwd);
    for (k, v) in &cfg.env {
        builder.env(k, v);
    }
    builder.env("TERM", "xterm-256color");

    let child = slave.spawn_command(builder).context("spawn child")?;
    // Drop the slave so the master reader sees EOF once the child closes its fds.
    drop(slave);

    let child_pid = child.process_id();
    let mut killer = child.clone_killer();
    let reader = master.try_clone_reader().context("clone pty reader")?;
    let writer = master.take_writer().context("take pty writer")?;

    // --- helper threads ----------------------------------------------------
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();

    // Reader: PTY → bounded output channel. `blocking_send` parks the thread when
    // the channel is full, which stops draining the PTY and back-pressures the
    // child — the mechanism that keeps a slow viewer from forcing truncation.
    let (output_tx, mut output_rx) = mpsc::channel::<Vec<u8>>(OUTPUT_BOUND);
    {
        let mut reader = reader;
        std::thread::spawn(move || {
            let mut buf = [0u8; READ_BUF];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break; // core gone
                        }
                    }
                }
            }
        });
    }

    // Writer: mpsc → PTY.
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_thread = {
        let mut writer = writer;
        std::thread::spawn(move || {
            while let Some(bytes) = write_rx.blocking_recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        })
    };

    // Wait: reap the child → Cmd::ChildExited.
    {
        let cmd_tx = cmd_tx.clone();
        let mut child = child;
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = cmd_tx.send(Cmd::ChildExited);
        });
    }

    // --- socket listener ---------------------------------------------------
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("binding control socket {}", socket.display()))?;
    {
        let cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let cmd_tx = cmd_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_conn(stream, cmd_tx).await {
                                tracing::debug!(error = %e, "control connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "accept failed");
                        break;
                    }
                }
            }
        });
    }

    // Best-effort: a termination signal kills the child and tears down cleanly.
    {
        let cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            if let Ok(mut sig) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                sig.recv().await;
                let _ = cmd_tx.send(Cmd::Kill);
            }
        });
    }

    // --- core task ---------------------------------------------------------
    let mut parser = vt100::Parser::new(cfg.rows.max(1), cfg.cols.max(1), DEFAULT_SCROLLBACK);
    let mut subscribers: HashMap<u64, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let mut next_id: u64 = 0;

    // Once the PTY reader hits EOF its channel closes; `recv` then returns `None`
    // immediately, so disable that select arm to avoid a busy-loop while we wait
    // for the wait thread's `ChildExited`.
    let mut output_open = true;
    loop {
        tokio::select! {
            // `biased`: poll control before output so ping/resize/kill stay
            // responsive even while a burst of output is flowing.
            biased;
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Send(bytes) => {
                        let _ = write_tx.send(bytes);
                    }
                    Cmd::Resize(cols, rows) => {
                        apply_resize(&mut parser, &*master, cols, rows);
                    }
                    Cmd::Capture { history, resp } => {
                        let _ = resp.send(render(&mut parser, history));
                    }
                    Cmd::Ping { resp } => {
                        // The supervisor answers only while the child runs — it
                        // tears down on exit (the session simply disappears), so
                        // an answered ping always means alive.
                        let (rows, cols) = parser.screen().size();
                        let _ = resp.send(PongInfo {
                            alive: true,
                            pid: child_pid,
                            cols,
                            rows,
                            alternate_screen: parser.screen().alternate_screen(),
                        });
                    }
                    Cmd::Attach { cols, rows, out_tx, resp } => {
                        // Match the PTY to the attaching client so the app
                        // repaints at its size, then hand the client a full
                        // repaint of the current screen so it is correct even if
                        // the app does not redraw.
                        let (cur_rows, cur_cols) = parser.screen().size();
                        if (cols, rows) != (cur_cols, cur_rows) && cols > 0 && rows > 0 {
                            apply_resize(&mut parser, &*master, cols, rows);
                        }
                        let repaint = parser.screen().contents_formatted();
                        // The channel is fresh and empty, so this first frame fits.
                        let _ = out_tx.try_send(repaint);
                        let id = next_id;
                        next_id += 1;
                        subscribers.insert(id, out_tx);
                        let _ = resp.send(id);
                    }
                    Cmd::Detach(id) => {
                        subscribers.remove(&id);
                    }
                    Cmd::Kill => {
                        // Reap the *whole* agent, not just its top shell.
                        // portable_pty `setsid`s the child into its own session, so
                        // the child pid is also the process-group id: SIGKILL the
                        // negative pid to take down the shell and everything it
                        // spawned in one shot. `killer.kill()` alone hits only the
                        // leader, leaving group members that ignore the PTY's
                        // SIGHUP (detached helpers, daemons) running and orphaned to
                        // PID 1; SIGKILL can't be caught, so the teardown is total.
                        // Guard `pid != 0`: a real spawned child never has pid 0,
                        // but `kill(-0, …)` would target the supervisor's *own*
                        // process group, so rule the footgun out explicitly. ESRCH
                        // (group already gone) is success; anything else is worth a
                        // line.
                        if let Some(pid) = child_pid.filter(|&p| p != 0) {
                            if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
                                let err = std::io::Error::last_os_error();
                                if err.raw_os_error() != Some(libc::ESRCH) {
                                    tracing::warn!(pid, error = %err, "process-group kill failed");
                                }
                            }
                        }
                        let _ = killer.kill();
                        break;
                    }
                    Cmd::ChildExited => break,
                }
            }
            out = output_rx.recv(), if output_open => {
                match out {
                    Some(bytes) => {
                        parser.process(&bytes);
                        fan_out(&mut subscribers, &bytes).await;
                    }
                    None => output_open = false, // PTY closed; ChildExited follows
                }
            }
        }
    }

    // Teardown: drop subscribers (closing their pumps), stop the writer thread,
    // remove the socket so a future spawn for this name binds cleanly.
    drop(subscribers);
    drop(write_tx);
    let _ = writer_thread.join();
    let _ = std::fs::remove_file(&socket);
    tracing::info!(session = %cfg.name, "supervisor exited");
    Ok(())
}

/// Deliver one output chunk to every subscriber, applying back-pressure: each
/// send `await`s its viewer's bounded channel (so a slow viewer slows the read
/// side rather than losing bytes), but a viewer that accepts nothing for
/// [`EVICT_AFTER`] — or whose channel is closed — is dropped, so one wedged or
/// dead client cannot stall the child forever. With no subscribers this is a
/// no-op and the child runs free.
async fn fan_out(subscribers: &mut HashMap<u64, mpsc::Sender<Vec<u8>>>, bytes: &[u8]) {
    if subscribers.is_empty() {
        return;
    }
    let mut wedged = Vec::new();
    for (id, tx) in subscribers.iter() {
        match tokio::time::timeout(EVICT_AFTER, tx.send(bytes.to_vec())).await {
            Ok(Ok(())) => {}
            // Channel closed (client gone) or no capacity within EVICT_AFTER.
            _ => wedged.push(*id),
        }
    }
    for id in wedged {
        subscribers.remove(&id);
    }
}

/// Resize both the PTY (so the child gets SIGWINCH) and the emulator (so capture
/// and repaint reflect the new geometry). vt100 takes `(rows, cols)`.
fn apply_resize(
    parser: &mut vt100::Parser,
    master: &(dyn portable_pty::MasterPty + Send),
    cols: u16,
    rows: u16,
) {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let _ = master.resize(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    });
    parser.screen_mut().set_size(rows, cols);
}

/// Render the screen to text. `history` extra scrollback rows are included above
/// the visible screen (clamped to what the emulator retains).
fn render(parser: &mut vt100::Parser, history: u32) -> String {
    if history == 0 {
        return parser.screen().contents();
    }
    // Scroll the emulator's viewport up by `history` rows, capture, then restore.
    // `contents()` reads from the current scrollback offset.
    let want = history as usize;
    parser.screen_mut().set_scrollback(want);
    let text = parser.screen().contents();
    parser.screen_mut().set_scrollback(0);
    text
}

/// One control connection: a loop of request frames. Most ops reply on the same
/// stream and continue; [`ATTACH`](op::ATTACH) consumes the connection and
/// switches it to the live terminal stream.
async fn handle_conn(
    stream: tokio::net::UnixStream,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) -> Result<()> {
    let mut stream = stream;
    loop {
        let Some(frame) = protocol::read_frame(&mut stream).await? else {
            return Ok(()); // clean disconnect
        };
        match frame.op {
            op::CAPTURE => {
                let (resp_tx, resp_rx) = oneshot::channel();
                if cmd_tx
                    .send(Cmd::Capture {
                        history: frame.as_u32(),
                        resp: resp_tx,
                    })
                    .is_err()
                {
                    return Ok(());
                }
                let text = resp_rx.await.unwrap_or_default();
                protocol::write_frame(
                    &mut stream,
                    &Frame::new(op::CAPTURE_RESP, text.into_bytes()),
                )
                .await?;
            }
            op::SEND => {
                let _ = cmd_tx.send(Cmd::Send(frame.payload));
                protocol::write_frame(&mut stream, &Frame::new(op::OK, Vec::new())).await?;
            }
            op::RESIZE => {
                if let Some((cols, rows)) = frame.as_size() {
                    let _ = cmd_tx.send(Cmd::Resize(cols, rows));
                }
                protocol::write_frame(&mut stream, &Frame::new(op::OK, Vec::new())).await?;
            }
            op::PING => {
                let (resp_tx, resp_rx) = oneshot::channel();
                if cmd_tx.send(Cmd::Ping { resp: resp_tx }).is_err() {
                    return Ok(());
                }
                let info = resp_rx.await.ok();
                let payload = serde_json::to_vec(&info).unwrap_or_default();
                protocol::write_frame(&mut stream, &Frame::new(op::PONG, payload)).await?;
            }
            op::KILL => {
                let _ = cmd_tx.send(Cmd::Kill);
                protocol::write_frame(&mut stream, &Frame::new(op::OK, Vec::new())).await?;
                return Ok(());
            }
            op::ATTACH => {
                let (cols, rows) = frame.as_size().unwrap_or((80, 24));
                return handle_attach(stream, cmd_tx, cols, rows).await;
            }
            other => {
                protocol::write_frame(
                    &mut stream,
                    &Frame::new(op::ERR, format!("unknown opcode {other:#x}").into_bytes()),
                )
                .await?;
            }
        }
    }
}

/// Drive a live terminal attach: stream PTY output to the client and forward its
/// input/resize back, until either side closes. On exit, detach the subscriber.
async fn handle_attach(
    stream: tokio::net::UnixStream,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    cols: u16,
    rows: u16,
) -> Result<()> {
    let (mut rd, mut wr) = stream.into_split();

    // Register as a subscriber; the core task sends the initial repaint first.
    // Bounded so a wedged client is evicted (see `Cmd::Output`) instead of
    // growing the supervisor's memory without bound.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(SUBSCRIBER_BOUND);
    let (id_tx, id_rx) = oneshot::channel();
    cmd_tx
        .send(Cmd::Attach {
            cols,
            rows,
            out_tx,
            resp: id_tx,
        })
        .map_err(|_| anyhow::anyhow!("supervisor gone"))?;
    let sub_id = id_rx.await.context("attach registration dropped")?;

    // Output pump: core → client OUTPUT frames.
    let mut out_task = tokio::spawn(async move {
        while let Some(chunk) = out_rx.recv().await {
            if protocol::write_frame(&mut wr, &Frame::new(op::OUTPUT, chunk))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Input pump: client INPUT/RESIZE frames → core.
    let in_cmd_tx = cmd_tx.clone();
    let mut in_task = tokio::spawn(async move {
        while let Ok(Some(frame)) = protocol::read_frame(&mut rd).await {
            let cmd = match frame.op {
                op::INPUT => Cmd::Send(frame.payload),
                op::RESIZE => match frame.as_size() {
                    Some((c, r)) => Cmd::Resize(c, r),
                    None => continue,
                },
                _ => continue, // ignore stray control ops mid-stream
            };
            if in_cmd_tx.send(cmd).is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = &mut out_task => in_task.abort(),
        _ = &mut in_task => out_task.abort(),
    }
    let _ = cmd_tx.send(Cmd::Detach(sub_id));
    Ok(())
}
