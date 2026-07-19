//! The `tapestry` CLI: spawn, drive, and attach to terminal sessions.
//!
//! Doubles as the supervisor entry point — `tapestry supervise <json>` is the
//! detached process [`tapestry::spawn_detached`] launches; the other subcommands
//! are a thin shell over [`tapestry::Client`] for standalone use and manual
//! poking during development.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tapestry::{Client, LaunchOptions, LaunchSpec};

fn usage() -> ! {
    eprintln!(
        "tapestry — terminal session supervisor

usage:
  tapestry spawn   <name> <cwd> <script>   launch a detached session
  tapestry ls                              list live sessions
  tapestry alive   <name>                  exit 0 if the session is live
  tapestry capture <name> [history]        print the rendered screen
  tapestry send    <name> <text>           type text into the session
  tapestry resize  <name> <cols> <rows>    resize the session
  tapestry attach  <name>                  attach this terminal to the session
  tapestry kill    <name>                  kill the session
  tapestry supervise -                     (internal) run a supervisor; reads its JSON spec from stdin"
    );
    std::process::exit(2);
}

/// Reap exited children on every `SIGCHLD`. In a supervisor process the only
/// children are the agent and whatever it reparents to us — we are the session's
/// subreaper (see the `supervise` arm) — so `waitpid(-1)` over all of them is
/// correct: it keeps orphaned, detached helpers (a backgrounded `gh`, `sleep`,
/// an MCP server) from lingering as zombies. The agent's own exit is still
/// detected inside `supervise`; if a reap here wins that race the wait there
/// just returns early and the exit is reported all the same.
#[cfg(target_os = "linux")]
fn reap_orphans() {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async {
        let Ok(mut sigchld) = signal(SignalKind::child()) else {
            tracing::warn!("cannot watch SIGCHLD; orphaned children may linger as zombies");
            return;
        };
        loop {
            sigchld.recv().await;
            // Drain every child that has exited. `WNOHANG` so we never block the
            // task; 0 (none ready) or -1 (no children) both mean "done — wait
            // for the next SIGCHLD".
            loop {
                let mut status = 0;
                let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                if pid <= 0 {
                    break;
                }
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    match cmd {
        "supervise" => {
            // The spec normally arrives on **stdin** (`supervise -`), so its
            // secret env values never land on argv (world-readable via `ps` /
            // /proc/<pid>/cmdline). An explicit JSON arg is still accepted for
            // manual poking. See [`tapestry::spawn_detached`].
            let spec: LaunchSpec = match args.get(1).map(String::as_str) {
                Some("-") | None => {
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .context("reading supervise spec from stdin")?;
                    serde_json::from_str(&buf).context("parsing supervise spec (stdin)")?
                }
                Some(json) => {
                    serde_json::from_str(json).context("parsing supervise spec (argv)")?
                }
            };
            // This process supervises exactly one session, so make it the
            // session's subreaper: anything the agent orphans (it backgrounds
            // gh, sleep, MCP servers, … which then detach) reparents here
            // instead of escaping up to loom / PID 1, and `reap_orphans` waits
            // on it. Per-session cleanup that holds whether or not loom runs
            // under a container init — the standalone-binary case has no init,
            // and a containerised loom that *is* PID 1 never sees these orphans.
            #[cfg(target_os = "linux")]
            {
                unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
                reap_orphans();
            }
            tapestry::supervise(spec.into()).await?;
        }
        "spawn" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            let cwd = args.get(2).unwrap_or_else(|| usage());
            let script = args.get(3).unwrap_or_else(|| usage());
            tapestry::spawn_detached(&LaunchOptions {
                name,
                cwd: &PathBuf::from(cwd),
                script,
                env: &[],
                cols: 80,
                rows: 24,
                mode: tapestry::Mode::Pty,
                segment_max_bytes: None,
                supervisor_bin: None, // current_exe is the tapestry binary
            })
            .await?;
            println!("{name}");
        }
        "ls" => {
            for name in tapestry::list_sessions().await {
                println!("{name}");
            }
        }
        "alive" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            if !Client::is_alive(name).await {
                std::process::exit(1);
            }
        }
        "capture" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            let history: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let mut c = Client::connect(name).await?;
            print!("{}", c.capture(history).await?);
        }
        "send" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            let text = args.get(2).unwrap_or_else(|| usage());
            let mut c = Client::connect(name).await?;
            c.send(text.as_bytes()).await?;
        }
        "resize" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            let cols: u16 = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            let rows: u16 = args
                .get(3)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            let mut c = Client::connect(name).await?;
            c.resize(cols, rows).await?;
        }
        "kill" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            let mut c = Client::connect(name).await?;
            c.kill().await?;
        }
        "attach" => {
            let name = args.get(1).unwrap_or_else(|| usage());
            attach_terminal(name).await?;
        }
        _ => usage(),
    }
    Ok(())
}

/// Attach the controlling terminal to a session: put the tty in raw mode, stream
/// PTY output to stdout, forward stdin to the session. Ctrl-] detaches (the
/// child keeps running). A convenience for standalone use — loom bridges attach
/// over a WebSocket instead.
async fn attach_terminal(name: &str) -> Result<()> {
    let client = Client::connect(name).await?;
    let (cols, rows) = term_size().unwrap_or((80, 24));
    let mut attach = client.attach(cols, rows).await?;

    let _raw = RawMode::enable();
    eprintln!("[tapestry] attached to {name} — Ctrl-] to detach\r");

    // stdin → session, on a blocking thread (stdin reads block).
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Ctrl-] (0x1d) detaches.
                    if buf[..n].contains(&0x1d) {
                        let _ = in_tx.send(Vec::new()); // signal detach
                        break;
                    }
                    if in_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut stdout = std::io::stdout();
    loop {
        tokio::select! {
            out = attach.recv() => match out {
                Some(chunk) => { stdout.write_all(&chunk)?; stdout.flush()?; }
                None => break,
            },
            msg = in_rx.recv() => match msg {
                Some(bytes) if bytes.is_empty() => break, // Ctrl-] detach
                Some(bytes) => { attach.send_input(&bytes).await?; }
                None => break,
            },
        }
    }
    eprintln!("\r\n[tapestry] detached\r");
    Ok(())
}

/// The controlling terminal's `(cols, rows)` via TIOCGWINSZ.
fn term_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(std::io::stdin().as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

/// RAII raw-mode for the controlling terminal; restores cooked mode on drop.
struct RawMode {
    fd: i32,
    prev: libc::termios,
}

impl RawMode {
    fn enable() -> Option<Self> {
        let fd = std::io::stdin().as_raw_fd();
        unsafe {
            let mut prev: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut prev) != 0 {
                return None;
            }
            let mut raw = prev;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(RawMode { fd, prev })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.prev);
        }
    }
}
