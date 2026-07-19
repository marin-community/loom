//! End-to-end lifecycle tests for the in-process supervisor: spawn → capture →
//! send → resize → kill, plus reattach replay. The detached-process path
//! (`spawn_detached`) is exercised separately in `detached.rs`.
//!
//! Each test pins `WEAVER_HOME` to a private temp dir so sockets never collide
//! with a real weaver install or another test.

use std::time::Duration;

use serial_test::serial;
use tapestry::{Client, SupervisorConfig};
use tempfile::TempDir;

/// Run a supervisor in a background task for the duration of a test. Returns the
/// temp home (kept alive) and the session name.
struct Harness {
    _home: TempDir,
    name: String,
}

impl Harness {
    async fn start(tag: &str, script: &str) -> Harness {
        let home = TempDir::new().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        let name = format!("tap-test-{}-{tag}", std::process::id());
        let cfg = SupervisorConfig {
            name: name.clone(),
            cwd: std::env::temp_dir(),
            script: script.to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Pty,
            segment_max_bytes: None,
        };
        tokio::spawn(async move {
            let _ = tapestry::supervise(cfg).await;
        });
        // Wait for the socket to accept.
        for _ in 0..200 {
            if Client::connect(&name).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Harness { _home: home, name }
    }
}

/// Poll `capture` until the screen contains `needle` or the deadline passes.
async fn wait_for_screen(name: &str, needle: &str) -> String {
    for _ in 0..200 {
        if let Ok(mut c) = Client::connect(name).await {
            if let Ok(text) = c.capture(0).await {
                if text.contains(needle) {
                    return text;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("screen never contained {needle:?}");
}

#[tokio::test]
#[serial]
async fn capture_sees_child_output() {
    let h = Harness::start("capture", "echo HELLO_TAPESTRY; exec sleep 30").await;
    let screen = wait_for_screen(&h.name, "HELLO_TAPESTRY").await;
    assert!(screen.contains("HELLO_TAPESTRY"), "got: {screen:?}");

    // is_alive is true while the child runs.
    assert!(Client::is_alive(&h.name).await);

    // Kill ends it; is_alive flips to false once the supervisor tears down.
    Client::connect(&h.name)
        .await
        .unwrap()
        .kill()
        .await
        .unwrap();
    for _ in 0..100 {
        if !Client::is_alive(&h.name).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("session still alive after kill");
}

#[tokio::test]
#[serial]
async fn kill_reaps_the_whole_process_group() {
    // A reap must take down everything the agent spawned, not just its top
    // shell. portable_pty puts the child in its own session, so the supervisor
    // SIGKILLs the entire process group. Spawn a backgrounded grandchild that
    // *ignores* SIGHUP (as detached agent helpers do), so only the group-wide
    // SIGKILL — not the PTY hangup the supervisor's exit delivers — can stop it,
    // then confirm it is gone after the kill.
    let pidfile = std::env::temp_dir().join(format!("tap-pg-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);
    let script = format!(
        "nohup sleep 300 >/dev/null 2>&1 & echo $! > {}; exec sleep 300",
        pidfile.display()
    );
    let h = Harness::start("pgroup", &script).await;

    // Wait for the grandchild to record its pid.
    let mut pid = 0i32;
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(&pidfile) {
            if let Ok(p) = s.trim().parse::<i32>() {
                pid = p;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(pid > 0, "grandchild never recorded its pid");
    assert!(alive(pid), "grandchild should be running before the reap");

    Client::connect(&h.name)
        .await
        .unwrap()
        .kill()
        .await
        .unwrap();

    let mut gone = false;
    for _ in 0..200 {
        if !alive(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = std::fs::remove_file(&pidfile);
    assert!(
        gone,
        "grandchild {pid} survived the reap — orphaned, not killed"
    );
}

/// Whether `pid` still exists. `kill(pid, 0)` delivers no signal — it only
/// probes: `Ok` ⇒ alive (or a not-yet-reaped zombie), `EPERM` ⇒ alive but not
/// ours to signal, `ESRCH` ⇒ gone.
fn alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[tokio::test]
#[serial]
async fn send_reaches_the_shell() {
    // A bare shell; type a command and confirm its output appears on screen.
    let h = Harness::start("send", "exec sh").await;
    // Let the shell come up.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut c = Client::connect(&h.name).await.unwrap();
    c.send(b"echo SENT_OK\n").await.unwrap();
    let screen = wait_for_screen(&h.name, "SENT_OK").await;
    assert!(screen.contains("SENT_OK"), "got: {screen:?}");
    let _ = Client::connect(&h.name).await.unwrap().kill().await;
}

#[tokio::test]
#[serial]
async fn attach_replays_current_screen() {
    let h = Harness::start("replay", "echo REPLAY_MARKER; exec sleep 30").await;
    wait_for_screen(&h.name, "REPLAY_MARKER").await;

    // A fresh attach must receive a repaint that includes the existing screen
    // content, even though the marker was printed before we attached.
    let client = Client::connect(&h.name).await.unwrap();
    let mut attach = client.attach(80, 24).await.unwrap();
    let mut painted = String::new();
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(100), attach.recv()).await {
            Ok(Some(chunk)) => {
                painted.push_str(&String::from_utf8_lossy(&chunk));
                if painted.contains("REPLAY_MARKER") {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        painted.contains("REPLAY_MARKER"),
        "repaint missing marker; got: {painted:?}"
    );
    let _ = Client::connect(&h.name).await.unwrap().kill().await;
}

#[tokio::test]
#[serial]
async fn resize_updates_reported_size() {
    let h = Harness::start("resize", "exec sleep 30").await;
    let mut c = Client::connect(&h.name).await.unwrap();
    c.resize(120, 40).await.unwrap();
    let info = c.ping().await.unwrap();
    assert_eq!(
        (info.cols, info.rows),
        (120, 40),
        "size should reflect resize"
    );
    let _ = Client::connect(&h.name).await.unwrap().kill().await;
}

#[tokio::test]
#[serial]
async fn dead_session_is_not_alive() {
    let home = TempDir::new().unwrap();
    std::env::set_var("WEAVER_HOME", home.path());
    assert!(!Client::is_alive("nonexistent-session").await);
}
