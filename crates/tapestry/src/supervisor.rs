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
    /// Initial PTY size (relay mode ignores it — there is no PTY).
    pub cols: u16,
    pub rows: u16,
    /// Which backend to run: PTY + vt100 screen, or the ACP frame relay.
    pub mode: crate::Mode,
    /// (Relay) Spool segment-rotation threshold in bytes; `None` = default.
    pub segment_max_bytes: Option<u64>,
}

/// Create the run directory and clear a stale socket for `name`, returning the
/// control-socket path to bind. Shared by both backends. A *live* supervisor
/// already listening is refused rather than clobbered.
async fn prepare_socket(name: &str) -> Result<PathBuf> {
    let socket = crate::paths::socket_path(name);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating run dir {}", parent.display()))?;
    }
    // A stale socket from a prior crash would make bind() fail with EADDRINUSE.
    // A successful connect proves a live supervisor is already listening for this
    // name — refuse rather than clobber it; otherwise the file is stale, remove it.
    if socket.exists() {
        if tokio::net::UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!("session {name} already has a live supervisor");
        }
        let _ = std::fs::remove_file(&socket);
    }
    Ok(socket)
}

/// Run the supervisor to completion, dispatching on the backend mode.
pub async fn run(cfg: SupervisorConfig) -> Result<()> {
    match cfg.mode {
        crate::Mode::Pty => run_pty(cfg).await,
        crate::Mode::Relay => relay::run(cfg).await,
    }
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

/// Run a PTY supervisor to completion: bring up the PTY + child, serve the
/// control socket, and return once the child exits or a [`Kill`](Cmd::Kill)
/// arrives. The socket file is removed on the way out.
async fn run_pty(cfg: SupervisorConfig) -> Result<()> {
    let socket = prepare_socket(&cfg.name).await?;

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
                            // PTY sessions are not relays; the relay-only fields
                            // stay at their defaults.
                            relay: false,
                            exited: None,
                            spooled: 0,
                            acked: 0,
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

/// The **relay** backend: pipe the child's stdio (no PTY, no vt100) and act as a
/// durable frame relay between it and a possibly-absent, possibly-reconnecting
/// subscriber.
///
/// Shape mirrors the PTY supervisor — a single core task owns the mutable state
/// (the on-disk [`Spool`] and the one subscriber), fed by helper threads:
///
/// * a **reader thread** splits child stdout into newline-delimited frames (no
///   JSON parsing — framing only) and `blocking_send`s them into a bounded
///   channel, so a slow subscriber back-pressures the child;
/// * a **stderr thread** appends child stderr to a per-session log file;
/// * a **writer thread** drains client `WRITE`s into the child's stdin;
/// * a **wait thread** reaps the child and reports its exit code.
///
/// Unlike the PTY supervisor, the core does **not** shut down when the child
/// exits: it records the status, tells the live subscriber (after the last
/// frame), and keeps serving so the spool and stderr log stay queryable and a
/// late client can still subscribe/replay. Only an explicit `KILL` (or SIGTERM)
/// tears it down.
mod relay {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Stdio;

    /// Default spool segment-rotation threshold (4 MiB): enough headroom that
    /// rotation is rare, small enough that a fully-acked segment frees promptly.
    const DEFAULT_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
    /// Frame backlog buffered from the reader thread before its `blocking_send`
    /// parks and back-pressures the child — the relay analogue of `OUTPUT_BOUND`.
    const FRAME_BOUND: usize = 256;
    /// stderr read-chunk size.
    const STDERR_BUF: usize = 32 * 1024;

    /// A server→client message bound for the live subscriber.
    enum Out {
        Frame { seq: u64, body: Vec<u8> },
        Exit { code: i32 },
    }

    /// Control messages to the relay core (the single owner of the spool +
    /// subscriber). Spooled frames travel on their own bounded channel (so the
    /// reader thread can park under back-pressure), not through here.
    enum Cmd {
        /// Append bytes to the child's stdin.
        Write(Vec<u8>),
        /// Advance the retention watermark.
        Ack(u64),
        /// Begin streaming: replay every spooled frame with `seq > cursor`, then
        /// register as the live subscriber (evicting any previous one). Replies
        /// with the subscriber id once replay + registration finish.
        Subscribe {
            cursor: u64,
            out_tx: mpsc::Sender<Out>,
            resp: oneshot::Sender<u64>,
        },
        /// A subscriber's connection ended; clear it if still current.
        Unsubscribe(u64),
        /// A short textual status summary (the relay's `CAPTURE` answer).
        Capture { resp: oneshot::Sender<String> },
        /// Liveness + info.
        Ping { resp: oneshot::Sender<PongInfo> },
        /// Kill the child's process group and shut the supervisor down.
        Kill,
        /// The child exited with this code (a signal death is `128 + signal`).
        ChildExited(i32),
    }

    /// Run a relay supervisor to completion. Returns on `KILL`/SIGTERM (an
    /// on-its-own child exit does *not* return — see the type docs).
    pub async fn run(cfg: SupervisorConfig) -> Result<()> {
        let socket = super::prepare_socket(&cfg.name).await?;
        let segment_max = cfg
            .segment_max_bytes
            .unwrap_or(DEFAULT_SEGMENT_BYTES)
            .max(1);

        // --- child over pipes ---------------------------------------------
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&cfg.script)
            .current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        // Lead a new session so the child is its own process-group leader — then
        // `KILL` can SIGKILL the whole group (the adapter and anything it spawns),
        // exactly as the PTY path relies on portable_pty's setsid.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().context("spawning relay child")?;
        let child_pid: Option<u32> = Some(child.id());
        let mut stdin = child.stdin.take().context("relay child stdin missing")?;
        let stdout = child.stdout.take().context("relay child stdout missing")?;
        let stderr = child.stderr.take().context("relay child stderr missing")?;

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();

        // Reader: child stdout → newline frames → bounded channel.
        let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<u8>>(FRAME_BOUND);
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break, // EOF or read error
                    Ok(_) => {
                        // Strip the delimiter; forward any non-empty line verbatim
                        // (the relay never parses the JSON — framing only).
                        let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
                        if line.is_empty() {
                            continue;
                        }
                        if frame_tx.blocking_send(line.to_vec()).is_err() {
                            break; // core gone
                        }
                    }
                }
            }
        });

        // stderr: child stderr → per-session log, created fresh on spawn.
        let stderr_path = crate::paths::stderr_log_path(&cfg.name);
        let stderr_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&stderr_path)
            .with_context(|| format!("creating stderr log {}", stderr_path.display()))?;
        std::thread::spawn(move || {
            let mut reader = stderr;
            let mut file = stderr_file;
            let mut buf = [0u8; STDERR_BUF];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if file.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = file.flush();
                    }
                }
            }
        });

        // Writer: mpsc → child stdin.
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer_thread = std::thread::spawn(move || {
            while let Some(bytes) = write_rx.blocking_recv() {
                if stdin.write_all(&bytes).is_err() {
                    break;
                }
                let _ = stdin.flush();
            }
        });

        // Wait: reap the child → Cmd::ChildExited(code).
        {
            let cmd_tx = cmd_tx.clone();
            std::thread::spawn(move || {
                let code = child.wait().map(exit_code).unwrap_or(-1);
                let _ = cmd_tx.send(Cmd::ChildExited(code));
            });
        }

        // --- socket listener ----------------------------------------------
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
                                    tracing::debug!(error = %e, "relay control connection ended");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "relay accept failed");
                            break;
                        }
                    }
                }
            });
        }

        // SIGTERM tears down (kills the child) like the PTY path.
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

        // --- core task ----------------------------------------------------
        let mut spool = Spool::create(crate::paths::spool_dir(&cfg.name), segment_max)
            .context("creating frame spool")?;
        let mut subscriber: Option<(u64, mpsc::Sender<Out>)> = None;
        let mut next_sub_id: u64 = 0;
        let mut exited: Option<i32> = None;
        let mut frame_open = true;
        // Set once the child has exited *and* its stdout has fully drained into
        // the spool — the point after which `EXIT` may be delivered (so it always
        // trails the child's last frame, live or replayed).
        let mut finished = false;

        loop {
            tokio::select! {
                // Poll control ahead of frames so ping/kill stay responsive.
                biased;
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Cmd::Write(bytes) => {
                            let _ = write_tx.send(bytes);
                        }
                        Cmd::Ack(seq) => {
                            spool.set_ack(seq);
                        }
                        Cmd::Subscribe { cursor, out_tx, resp } => {
                            // A new subscriber evicts the previous one.
                            subscriber = None;
                            let mut wedged = false;
                            // Replay spooled frames strictly after the cursor,
                            // segment by segment (bounding memory to one segment).
                            'replay: for path in spool.replay_segments(cursor) {
                                for (seq, body) in read_records(&path) {
                                    if seq > cursor
                                        && !deliver(&out_tx, Out::Frame { seq, body }).await
                                    {
                                        wedged = true;
                                        break 'replay;
                                    }
                                }
                            }
                            // If the child is already fully done, the late
                            // subscriber still learns the terminal state.
                            if !wedged && finished {
                                if let Some(code) = exited {
                                    if !deliver(&out_tx, Out::Exit { code }).await {
                                        wedged = true;
                                    }
                                }
                            }
                            let id = next_sub_id;
                            next_sub_id += 1;
                            if !wedged {
                                subscriber = Some((id, out_tx));
                            }
                            let _ = resp.send(id);
                        }
                        Cmd::Unsubscribe(id) => {
                            if matches!(&subscriber, Some((cur, _)) if *cur == id) {
                                subscriber = None;
                            }
                        }
                        Cmd::Capture { resp } => {
                            let _ = resp.send(spool.summary(&cfg.name, child_pid, exited));
                        }
                        Cmd::Ping { resp } => {
                            let _ = resp.send(PongInfo {
                                // The relay supervisor answers even after the
                                // child exits, so `alive` reports *supervisor*
                                // liveness; `exited` carries the child's state.
                                alive: true,
                                pid: child_pid,
                                cols: 0,
                                rows: 0,
                                alternate_screen: false,
                                relay: true,
                                exited,
                                spooled: spool.spooled(),
                                acked: spool.acked(),
                            });
                        }
                        Cmd::Kill => {
                            kill_group(child_pid);
                            break;
                        }
                        Cmd::ChildExited(code) => {
                            if exited.is_none() {
                                exited = Some(code);
                            }
                            maybe_finish(&mut subscriber, &mut finished, exited, frame_open).await;
                        }
                    }
                }
                frame = frame_rx.recv(), if frame_open => {
                    match frame {
                        Some(bytes) => match spool.append(&bytes) {
                            Ok(seq) => {
                                if let Some((_, tx)) = subscriber.as_ref() {
                                    if !deliver(tx, Out::Frame { seq, body: bytes }).await {
                                        subscriber = None;
                                    }
                                }
                            }
                            Err(e) => tracing::error!(error = %e, "spool append failed"),
                        },
                        None => {
                            // Child stdout closed: every frame is now spooled.
                            frame_open = false;
                            maybe_finish(&mut subscriber, &mut finished, exited, frame_open).await;
                        }
                    }
                }
            }
        }

        // Teardown: drop the subscriber, stop the writer, and remove this
        // session's artifacts (socket, spool, stderr log) — reached only on an
        // explicit kill, which destroys the session.
        drop(subscriber);
        drop(write_tx);
        let _ = writer_thread.join();
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir_all(spool.dir());
        let _ = std::fs::remove_file(&stderr_path);
        tracing::info!(session = %cfg.name, "relay supervisor exited");
        Ok(())
    }

    /// Deliver one message to the subscriber with back-pressure: `await` its
    /// bounded channel, but treat a wedged (no capacity within [`EVICT_AFTER`]) or
    /// closed channel as gone. Returns `false` if the subscriber should be dropped.
    async fn deliver(tx: &mpsc::Sender<Out>, msg: Out) -> bool {
        matches!(
            tokio::time::timeout(EVICT_AFTER, tx.send(msg)).await,
            Ok(Ok(()))
        )
    }

    /// Once the child has exited *and* its stdout has drained (`!frame_open`),
    /// deliver the one-time `EXIT` to the live subscriber and latch `finished`.
    /// Idempotent — the latch means it fires exactly once.
    async fn maybe_finish(
        subscriber: &mut Option<(u64, mpsc::Sender<Out>)>,
        finished: &mut bool,
        exited: Option<i32>,
        frame_open: bool,
    ) {
        if *finished || frame_open {
            return;
        }
        let Some(code) = exited else { return };
        *finished = true;
        if let Some((_, tx)) = subscriber.as_ref() {
            if !deliver(tx, Out::Exit { code }).await {
                *subscriber = None;
            }
        }
    }

    /// SIGKILL the child's whole process group (see the PTY `Cmd::Kill` note for
    /// why the group, not just the leader). ESRCH (group already gone) is success.
    fn kill_group(pid: Option<u32>) {
        if let Some(pid) = pid.filter(|&p| p != 0) {
            if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    tracing::warn!(pid, error = %err, "relay process-group kill failed");
                }
            }
        }
    }

    /// Map a child exit to a single code: the exit status, or `128 + signal` for
    /// a signal death (the shell convention), so it is always `Some` for
    /// [`PongInfo::exited`].
    fn exit_code(status: std::process::ExitStatus) -> i32 {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }

    /// One relay control connection: a loop of request frames. `PING`/`CAPTURE`/
    /// `KILL`/`WRITE`/`ACK` reply on the same stream and continue; `SUBSCRIBE`
    /// consumes the connection and switches it to the live frame stream.
    async fn handle_conn(
        stream: tokio::net::UnixStream,
        cmd_tx: mpsc::UnboundedSender<Cmd>,
    ) -> Result<()> {
        let mut stream = stream;
        loop {
            let Some(frame) = protocol::read_frame(&mut stream).await? else {
                return Ok(());
            };
            match frame.op {
                op::PING => {
                    let (resp_tx, resp_rx) = oneshot::channel();
                    if cmd_tx.send(Cmd::Ping { resp: resp_tx }).is_err() {
                        return Ok(());
                    }
                    let info = resp_rx.await.ok();
                    let payload = serde_json::to_vec(&info).unwrap_or_default();
                    protocol::write_frame(&mut stream, &Frame::new(op::PONG, payload)).await?;
                }
                op::CAPTURE => {
                    let (resp_tx, resp_rx) = oneshot::channel();
                    if cmd_tx.send(Cmd::Capture { resp: resp_tx }).is_err() {
                        return Ok(());
                    }
                    let text = resp_rx.await.unwrap_or_default();
                    protocol::write_frame(
                        &mut stream,
                        &Frame::new(op::CAPTURE_RESP, text.into_bytes()),
                    )
                    .await?;
                }
                op::WRITE => {
                    let _ = cmd_tx.send(Cmd::Write(frame.payload));
                    protocol::write_frame(&mut stream, &Frame::new(op::OK, Vec::new())).await?;
                }
                op::ACK => {
                    let _ = cmd_tx.send(Cmd::Ack(frame.as_u64()));
                    protocol::write_frame(&mut stream, &Frame::new(op::OK, Vec::new())).await?;
                }
                op::KILL => {
                    let _ = cmd_tx.send(Cmd::Kill);
                    protocol::write_frame(&mut stream, &Frame::new(op::OK, Vec::new())).await?;
                    return Ok(());
                }
                op::SUBSCRIBE => {
                    return handle_subscribe(stream, cmd_tx, frame.as_u64()).await;
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

    /// Drive a live subscribe: stream `FRAME`/`EXIT` to the client and forward its
    /// `ACK`/`WRITE` back, until either side closes. The output pump is spawned
    /// *before* registration so the core's replay sends have a consumer.
    async fn handle_subscribe(
        stream: tokio::net::UnixStream,
        cmd_tx: mpsc::UnboundedSender<Cmd>,
        cursor: u64,
    ) -> Result<()> {
        let (rd, wr) = stream.into_split();
        let (out_tx, mut out_rx) = mpsc::channel::<Out>(SUBSCRIBER_BOUND);

        // Output pump: core → client FRAME/EXIT frames.
        let mut out_task = tokio::spawn(async move {
            let mut wr = wr;
            while let Some(msg) = out_rx.recv().await {
                let frame = match msg {
                    Out::Frame { seq, body } => Frame::relay_frame(seq, &body),
                    Out::Exit { code } => Frame::new(
                        op::EXIT,
                        serde_json::to_vec(&Some(code)).unwrap_or_default(),
                    ),
                };
                if protocol::write_frame(&mut wr, &frame).await.is_err() {
                    break;
                }
            }
        });

        // Register; replay runs inside the core before it replies with our id.
        let (id_tx, id_rx) = oneshot::channel();
        cmd_tx
            .send(Cmd::Subscribe {
                cursor,
                out_tx,
                resp: id_tx,
            })
            .map_err(|_| anyhow::anyhow!("supervisor gone"))?;
        let sub_id = id_rx.await.context("subscribe registration dropped")?;

        // Input pump: client ACK/WRITE frames → core.
        let in_cmd_tx = cmd_tx.clone();
        let mut in_task = tokio::spawn(async move {
            let mut rd = rd;
            while let Ok(Some(frame)) = protocol::read_frame(&mut rd).await {
                let cmd = match frame.op {
                    op::ACK => Cmd::Ack(frame.as_u64()),
                    op::WRITE => Cmd::Write(frame.payload),
                    _ => continue, // ignore stray ops mid-stream
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
        let _ = cmd_tx.send(Cmd::Unsubscribe(sub_id));
        Ok(())
    }

    /// The zero-padded segment filename for a segment whose first frame is `seq`.
    fn segment_name(first_seq: u64) -> String {
        format!("{first_seq:08}.seg")
    }

    /// Parse a segment file into its `(seq, frame)` records. One record per line,
    /// `seq\tframe`; the split is on the *first* tab so a frame containing tabs is
    /// preserved. A missing/short file yields no records.
    fn read_records(path: &Path) -> Vec<(u64, Vec<u8>)> {
        let Ok(data) = std::fs::read(path) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Some(tab) = line.iter().position(|&b| b == b'\t') else {
                continue;
            };
            let Ok(seq) = std::str::from_utf8(&line[..tab])
                .unwrap_or("")
                .parse::<u64>()
            else {
                continue;
            };
            out.push((seq, line[tab + 1..].to_vec()));
        }
        out
    }

    /// One rotated-out or active segment's metadata. The first seq is encoded in
    /// the file name (`<first-seq>.seg`), so only the moving `last_seq` is tracked.
    struct Segment {
        /// Highest seq written; the segment's first-seq minus one while empty.
        last_seq: u64,
        path: PathBuf,
    }

    /// The on-disk frame spool: an append-only directory of segment files, one
    /// `seq\tframe\n` record per line, rotated by size. Crash-tolerant on the
    /// writer side (append-only); it need only outlive *client* restarts, so the
    /// index (segment list, next seq, watermark) is kept in memory rather than
    /// recovered — a supervisor restart is out of scope.
    struct Spool {
        dir: PathBuf,
        segment_max_bytes: u64,
        /// Seq the next appended frame will get (starts at 1).
        next_seq: u64,
        /// Retention watermark — the highest acked seq.
        ack: u64,
        /// Rotated-out (sealed) segments, oldest first.
        sealed: Vec<Segment>,
        /// The segment currently being appended to.
        active: Segment,
        active_file: std::fs::File,
        active_bytes: u64,
    }

    impl Spool {
        /// Create a fresh spool directory (clearing any stale one from a prior
        /// session of the same name) with an empty first segment at seq 1.
        fn create(dir: PathBuf, segment_max_bytes: u64) -> Result<Self> {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating spool dir {}", dir.display()))?;
            let first_seq = 1;
            let path = dir.join(segment_name(first_seq));
            let active_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening spool segment {}", path.display()))?;
            Ok(Self {
                dir,
                segment_max_bytes,
                next_seq: 1,
                ack: 0,
                sealed: Vec::new(),
                active: Segment {
                    last_seq: first_seq - 1,
                    path,
                },
                active_file,
                active_bytes: 0,
            })
        }

        /// Append one frame, returning its seq. Rotates to a fresh segment once
        /// the active one crosses the size threshold.
        fn append(&mut self, body: &[u8]) -> Result<u64> {
            let seq = self.next_seq;
            let mut rec = Vec::with_capacity(20 + body.len());
            rec.extend_from_slice(seq.to_string().as_bytes());
            rec.push(b'\t');
            rec.extend_from_slice(body);
            rec.push(b'\n');
            self.active_file.write_all(&rec)?;
            self.active.last_seq = seq;
            self.active_bytes += rec.len() as u64;
            self.next_seq += 1;
            if self.active_bytes >= self.segment_max_bytes {
                self.rotate()?;
            }
            Ok(seq)
        }

        /// Seal the active segment and open a fresh one starting at `next_seq`.
        fn rotate(&mut self) -> Result<()> {
            let first_seq = self.next_seq;
            let path = self.dir.join(segment_name(first_seq));
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening spool segment {}", path.display()))?;
            let sealed = std::mem::replace(
                &mut self.active,
                Segment {
                    last_seq: first_seq - 1,
                    path,
                },
            );
            self.sealed.push(sealed);
            self.active_file = file;
            self.active_bytes = 0;
            Ok(())
        }

        /// Advance the watermark and delete every sealed segment fully covered by
        /// it (highest seq ≤ ack). The active segment is never deleted. An
        /// over-ack (beyond what's spooled) clamps to the last spooled seq.
        fn set_ack(&mut self, seq: u64) {
            let seq = seq.min(self.next_seq.saturating_sub(1));
            if seq <= self.ack {
                return;
            }
            self.ack = seq;
            let ack = self.ack;
            self.sealed.retain(|seg| {
                if seg.last_seq <= ack {
                    let _ = std::fs::remove_file(&seg.path);
                    false
                } else {
                    true
                }
            });
        }

        /// Segment file paths (in seq order) that may hold a frame with
        /// `seq > cursor` — every segment whose highest seq exceeds the cursor.
        fn replay_segments(&self, cursor: u64) -> Vec<PathBuf> {
            let mut paths = Vec::new();
            for seg in &self.sealed {
                if seg.last_seq > cursor {
                    paths.push(seg.path.clone());
                }
            }
            if self.active.last_seq > cursor {
                paths.push(self.active.path.clone());
            }
            paths
        }

        /// The highest spooled seq (0 before the first frame).
        fn spooled(&self) -> u64 {
            self.next_seq.saturating_sub(1)
        }

        /// The retention watermark.
        fn acked(&self) -> u64 {
            self.ack
        }

        fn dir(&self) -> &Path {
            &self.dir
        }

        /// A short human-readable status line — the relay's `CAPTURE` answer.
        fn summary(&self, name: &str, pid: Option<u32>, exited: Option<i32>) -> String {
            let child = match exited {
                Some(code) => format!("exited (code {code})"),
                None => match pid {
                    Some(p) => format!("running (pid {p})"),
                    None => "running".to_string(),
                },
            };
            let spooled = self.spooled();
            let retained = spooled.saturating_sub(self.ack);
            format!(
                "relay session {name}: child {child}; spooled seq 1..{spooled} \
                 ({retained} retained past ack {}), {} segment(s)\n",
                self.ack,
                self.sealed.len() + 1,
            )
        }
    }
}
