//! Integration tests for the **relay** backend: a piped-stdio supervisor with a
//! durable frame spool, subscribe/replay/ack/write, stderr capture, and exit
//! reporting. Mirrors `lifecycle.rs` — each test runs an in-process supervisor
//! against a private temp `WEAVER_HOME`, so sockets and spools never collide.
//!
//! The child in most tests is `while read l; do echo "got:$l"; done` — an echo
//! loop over stdin, exactly the shape the relay is built for (write a line in,
//! get a framed line out). dash flushes each `echo` promptly even when stdout is
//! a pipe, so no `stdbuf` dance is needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serial_test::serial;
use tapestry::{Client, Mode, RelayEvent, RelayStream, SupervisorConfig};
use tempfile::TempDir;

/// An echo loop: read a line, emit `got:<line>`.
const ECHO: &str = "while read l; do echo \"got:$l\"; done";

/// A running in-process relay supervisor pinned to a private temp home.
struct Relay {
    _home: TempDir,
    name: String,
}

impl Relay {
    async fn start(tag: &str, script: &str, segment_max_bytes: Option<u64>) -> Relay {
        let home = TempDir::new().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        let name = format!("tap-relay-{}-{tag}", std::process::id());
        let cfg = SupervisorConfig {
            name: name.clone(),
            cwd: std::env::temp_dir(),
            script: script.to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            mode: Mode::Relay,
            segment_max_bytes,
        };
        tokio::spawn(async move {
            let _ = tapestry::supervise(cfg).await;
        });
        for _ in 0..200 {
            if Client::connect(&name).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Relay { _home: home, name }
    }

    /// Append a newline-terminated line to the child's stdin.
    async fn write_line(&self, line: &str) {
        Client::connect(&self.name)
            .await
            .unwrap()
            .relay_write(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn subscribe(&self, cursor: u64) -> RelayStream {
        Client::connect(&self.name)
            .await
            .unwrap()
            .subscribe(cursor)
            .await
            .unwrap()
    }

    async fn ping(&self) -> tapestry::protocol::PongInfo {
        Client::connect(&self.name)
            .await
            .unwrap()
            .ping()
            .await
            .unwrap()
    }

    /// Block until the spool has assigned at least `n` sequence numbers.
    async fn wait_spooled(&self, n: u64) {
        for _ in 0..400 {
            if self.ping().await.spooled >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("spool never reached seq {n}");
    }

    async fn kill(&self) {
        if let Ok(mut c) = Client::connect(&self.name).await {
            let _ = c.kill().await;
        }
    }
}

/// The next event, expected to be a `Frame`; panics on anything else.
async fn next_frame(sub: &mut RelayStream) -> (u64, Vec<u8>) {
    match tokio::time::timeout(Duration::from_secs(5), sub.recv()).await {
        Ok(Some(RelayEvent::Frame { seq, payload })) => (seq, payload),
        other => panic!("expected a frame, got {other:?}"),
    }
}

/// The next event, expected to be `Exit`; panics on anything else.
async fn next_exit(sub: &mut RelayStream) -> Option<i32> {
    match tokio::time::timeout(Duration::from_secs(5), sub.recv()).await {
        Ok(Some(RelayEvent::Exit { status })) => status,
        other => panic!("expected exit, got {other:?}"),
    }
}

/// Segment files in a spool dir, sorted (so `[0]` is the earliest).
fn segment_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "seg"))
        .collect();
    v.sort();
    v
}

/// The highest seq recorded in a segment file (records are `seq\tframe`).
fn segment_max_seq(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split('\t').next().and_then(|s| s.parse::<u64>().ok()))
        .max()
        .unwrap()
}

/// A WRITE reaches the child, whose echoed line comes back as FRAME seq 1.
#[tokio::test]
#[serial]
async fn write_then_subscribe_yields_the_frame() {
    let r = Relay::start("basics", ECHO, None).await;
    r.write_line("hello").await;

    let mut sub = r.subscribe(0).await;
    assert_eq!(next_frame(&mut sub).await, (1, b"got:hello".to_vec()));

    r.kill().await;
}

/// Frames written with no subscriber are spooled and replay in order on a later
/// subscribe; a cursored subscribe replays only what follows the cursor.
#[tokio::test]
#[serial]
async fn replay_survives_client_death_and_respects_the_cursor() {
    let r = Relay::start("replay", ECHO, None).await;

    // Two lines with nobody listening.
    r.write_line("a").await;
    r.write_line("b").await;
    r.wait_spooled(2).await;

    {
        let mut sub = r.subscribe(0).await;
        assert_eq!(next_frame(&mut sub).await, (1, b"got:a".to_vec()));
        assert_eq!(next_frame(&mut sub).await, (2, b"got:b".to_vec()));
        // Dropping `sub` here unsubscribes — the "client died" case.
    }

    // A third line, then a subscribe past the first two: only frame 3 replays.
    r.write_line("c").await;
    r.wait_spooled(3).await;
    let mut sub = r.subscribe(2).await;
    assert_eq!(next_frame(&mut sub).await, (3, b"got:c".to_vec()));

    r.kill().await;
}

/// Acking past a sealed segment deletes its file; a subscribe from the watermark
/// still yields every frame after it.
#[tokio::test]
#[serial]
async fn ack_truncates_fully_acked_segments() {
    // A tiny segment size forces frequent rotation.
    let r = Relay::start("ack", ECHO, Some(20)).await;

    let total = 12u64;
    for i in 1..=total {
        r.write_line(&format!("l{i}")).await;
    }
    r.wait_spooled(total).await;

    let dir = tapestry::paths::spool_dir(&r.name);
    let segs = segment_files(&dir);
    assert!(
        segs.len() >= 2,
        "expected the spool to roll into multiple segments, got {segs:?}"
    );

    // Ack the whole first (sealed) segment, then watch its file disappear.
    let first = dir.join("00000001.seg");
    assert!(first.exists(), "first segment missing before ack");
    let ack_to = segment_max_seq(&first);
    Client::connect(&r.name)
        .await
        .unwrap()
        .relay_ack(ack_to)
        .await
        .unwrap();

    let mut deleted = false;
    for _ in 0..200 {
        if !first.exists() {
            deleted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        deleted,
        "first segment not deleted after acking seq {ack_to}"
    );

    // The ping watermark reflects the ack, and a subscribe from it replays the
    // whole tail — nothing lost to truncation.
    assert_eq!(r.ping().await.acked, ack_to);
    let mut sub = r.subscribe(ack_to).await;
    for seq in (ack_to + 1)..=total {
        assert_eq!(
            next_frame(&mut sub).await,
            (seq, format!("got:l{seq}").into_bytes())
        );
    }

    r.kill().await;
}

/// The child's stderr lands in the per-session log file.
#[tokio::test]
#[serial]
async fn stderr_is_captured_to_the_log() {
    let r = Relay::start("stderr", "echo OOPS_ON_STDERR >&2; exec sleep 30", None).await;
    let log = tapestry::paths::stderr_log_path(&r.name);

    let mut contents = String::new();
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(&log) {
            if s.contains("OOPS_ON_STDERR") {
                contents = s;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        contents.contains("OOPS_ON_STDERR"),
        "stderr log missing content; got {contents:?}"
    );

    r.kill().await;
}

/// When the child exits, the live subscriber gets an `Exit`, PING reports the
/// status while the supervisor stays alive, and the spool still replays.
#[tokio::test]
#[serial]
async fn exit_is_reported_and_the_spool_outlives_the_child() {
    let r = Relay::start("exit", "echo bye; exit 7", None).await;

    // Frame then exit (whether the frame streams live or replays from the spool).
    let mut sub = r.subscribe(0).await;
    assert_eq!(next_frame(&mut sub).await, (1, b"bye".to_vec()));
    assert_eq!(next_exit(&mut sub).await, Some(7));

    // The supervisor answers after the child is gone: alive (supervisor), relay,
    // exit code recorded.
    let info = r.ping().await;
    assert!(info.alive, "relay supervisor should outlive its child");
    assert!(info.relay);
    assert_eq!(info.exited, Some(7));

    // The spool remains queryable: a fresh subscribe replays the frame + exit.
    let mut sub2 = r.subscribe(0).await;
    assert_eq!(next_frame(&mut sub2).await, (1, b"bye".to_vec()));
    assert_eq!(next_exit(&mut sub2).await, Some(7));

    r.kill().await;
}
