//! The recovery property: a session launched with `spawn_detached` survives the
//! launching process exiting. We can't kill *this* test process, but the
//! detached supervisor is a separate `setsid` session leader that is reparented
//! to init once its launcher goes away, so its survival is observable: spawn it,
//! drop every handle, and confirm it is still answering and its screen is intact
//! — exactly what "restart loom, the agent keeps running" relies on.
//!
//! Requires the `tapestry` binary to be built (the test resolves it from the
//! same target dir via `CARGO_BIN_EXE_tapestry`).

use std::time::Duration;

use serial_test::serial;
use tapestry::Client;
use tempfile::TempDir;

#[tokio::test]
#[serial]
async fn detached_supervisor_outlives_its_launcher() {
    let home = TempDir::new().unwrap();
    std::env::set_var("WEAVER_HOME", home.path());
    // The detached supervisor is the freshly-built `tapestry` binary, driven via
    // its `spawn` subcommand (which setsid-detaches a supervisor and returns). It
    // inherits our WEAVER_HOME.
    let bin = env!("CARGO_BIN_EXE_tapestry");
    let name = format!("tap-detach-{}", std::process::id());
    spawn_via_binary(bin, &name, "echo DETACHED_OK; exec sleep 60").await;

    // The supervisor is a separate process; this test holds no handle to it.
    // Confirm it is alive and its screen survived.
    for _ in 0..200 {
        if Client::is_alive(&name).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        Client::is_alive(&name).await,
        "detached supervisor not alive"
    );

    let mut painted = String::new();
    for _ in 0..200 {
        if let Ok(mut c) = Client::connect(&name).await {
            if let Ok(text) = c.capture(0).await {
                if text.contains("DETACHED_OK") {
                    painted = text;
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        painted.contains("DETACHED_OK"),
        "screen lost; got {painted:?}"
    );

    // Reattach and confirm the repaint carries the pre-existing content — the
    // "browser reconnects after a loom restart" path.
    let client = Client::connect(&name).await.unwrap();
    let mut attach = client.attach(80, 24).await.unwrap();
    let mut repaint = String::new();
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(100), attach.recv()).await {
            Ok(Some(chunk)) => {
                repaint.push_str(&String::from_utf8_lossy(&chunk));
                if repaint.contains("DETACHED_OK") {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        repaint.contains("DETACHED_OK"),
        "reattach repaint missing content; got {repaint:?}"
    );

    // Clean up the detached process.
    let _ = Client::connect(&name).await.unwrap().kill().await;
}

/// The supervisor is its session's subreaper: a process the agent detaches and
/// orphans must reparent to the supervisor (not escape to PID 1) and then get
/// reaped when it dies, instead of lingering as a zombie. Without this a
/// long-lived agent that shells out to detached helpers (`gh`, `sleep`, MCP
/// servers) accretes zombies for the life of the session.
#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn detached_supervisor_reaps_orphaned_descendants() {
    let home = TempDir::new().unwrap();
    std::env::set_var("WEAVER_HOME", home.path());
    let bin = env!("CARGO_BIN_EXE_tapestry");
    let name = format!("tap-reap-{}", std::process::id());

    // The subshell backgrounds `sleep` then exits immediately, orphaning it; with
    // the supervisor as subreaper the orphan reparents to the supervisor. The
    // foreground `sleep` keeps the session (and supervisor) alive meanwhile.
    let pidfile = std::env::temp_dir().join(format!("tap-reap-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);
    let script = format!(
        "(sleep 300 & echo $! > {}); exec sleep 300",
        pidfile.display()
    );
    spawn_via_binary(bin, &name, &script).await;

    // Read the orphan's pid.
    let mut orphan = String::new();
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(&pidfile) {
            if !s.trim().is_empty() {
                orphan = s.trim().to_string();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!orphan.is_empty(), "orphan pid never recorded");

    // It reparents to the supervisor (a `tapestry` process), not to init —
    // proof the subreaper attribute took effect.
    let mut reparented = false;
    for _ in 0..200 {
        if let Some(ppid) = proc_field(&orphan, 1) {
            if ppid != "1" && proc_comm(&ppid).as_deref() == Some("tapestry") {
                reparented = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        reparented,
        "orphan {orphan} did not reparent to the supervisor"
    );

    // Kill it; the supervisor must reap it — /proc/<pid> disappears rather than
    // sticking around as a zombie (state 'Z').
    if let Ok(pid) = orphan.parse::<i32>() {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let mut reaped = false;
    for _ in 0..200 {
        if proc_field(&orphan, 0).is_none() {
            reaped = true; // /proc entry gone → reaped
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = std::fs::remove_file(&pidfile);
    let _ = Client::connect(&name).await.unwrap().kill().await;
    assert!(
        reaped,
        "orphan {orphan} was not reaped — lingered as a zombie"
    );
}

/// Field `n` (0-based) of `/proc/<pid>/stat` *after* the parenthesised comm —
/// so field 0 is state, field 1 is ppid. `None` if the process is gone. Reading
/// after the last `)` sidesteps spaces/parens inside comm.
#[cfg(target_os = "linux")]
fn proc_field(pid: &str, n: usize) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(n).map(str::to_string)
}

/// `/proc/<pid>/comm` (the executable name), or `None` if the process is gone.
#[cfg(target_os = "linux")]
fn proc_comm(pid: &str) -> Option<String> {
    Some(
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()?
            .trim()
            .to_string(),
    )
}

/// Drive the `tapestry spawn` subcommand of the built binary, which setsid-
/// detaches a supervisor and returns immediately. Using the CLI (rather than the
/// library `spawn_detached`, which re-execs `current_exe`) makes the supervisor
/// the real `tapestry` binary even though we run inside the test harness.
async fn spawn_via_binary(bin: &str, name: &str, script: &str) {
    let status = tokio::process::Command::new(bin)
        .args([
            "spawn",
            name,
            &std::env::temp_dir().to_string_lossy(),
            script,
        ])
        .status()
        .await
        .expect("spawn tapestry");
    assert!(status.success(), "tapestry spawn failed");
}
