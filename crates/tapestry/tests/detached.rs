//! The recovery property: a session launched with `spawn_detached` survives the
//! launching process exiting. We can't kill *this* test process, but the
//! detached supervisor is a separate `setsid` process reparented to init, so its
//! survival is observable: spawn it, drop every handle, and confirm it is still
//! answering and its screen is intact — exactly what "restart loom, the agent
//! keeps running" relies on.
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
    // its `spawn` subcommand (which double-forks). It inherits our WEAVER_HOME.
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

/// Drive the `tapestry spawn` subcommand of the built binary, which double-forks
/// a detached supervisor and returns immediately. Using the CLI (rather than the
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
