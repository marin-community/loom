//! Manual container-runner lifecycle test.
//!
//! Build the test image with `HOST_UID=$(id -u)` and `HOST_GID=$(id -g)`, then
//! run `cargo build -p tapestry && cargo test -p loom --test docker_runner --
//! --ignored`. The regular suite stays Docker-free. This smoke covers placement,
//! launcher death, shared socket access, colocating a second supervisor, keeping
//! the operator shell on the launcher host, and a callback over the configured
//! sibling network.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serial_test::serial;

const CHILD_FLAG: &str = "LOOM_DOCKER_RUNNER_CHILD";
const ROOT_ENV: &str = "LOOM_DOCKER_RUNNER_TEST_ROOT";
const SESSION_ENV: &str = "LOOM_DOCKER_RUNNER_TEST_SESSION";
const NETWORK_ENV: &str = "LOOM_DOCKER_RUNNER_TEST_NETWORK";
const API_ENV: &str = "LOOM_DOCKER_RUNNER_TEST_API";

struct DockerFixture {
    api_container: String,
    network: String,
}

impl Drop for DockerFixture {
    fn drop(&mut self) {
        Command::new("docker")
            .args(["rm", "--force", &self.api_container])
            .status()
            .ok();
        Command::new("docker")
            .args(["network", "rm", &self.network])
            .status()
            .ok();
    }
}

fn configure(root: &Path, session: &str) {
    std::env::set_var("LOOM_RUNNER", "docker");
    std::env::set_var(
        "LOOM_SESSION_IMAGE",
        std::env::var("LOOM_DOCKER_TEST_IMAGE").unwrap_or_else(|_| "loom:latest".into()),
    );
    std::env::set_var("LOOM_SESSION_HOME_VOLUME", root.display().to_string());
    std::env::set_var(
        "LOOM_SESSION_UV_VOLUME",
        root.join("uv").display().to_string(),
    );
    std::env::set_var("LOOM_SESSION_DOCKER_GID", "0");
    std::env::set_var(
        "LOOM_SESSION_NETWORK",
        std::env::var(NETWORK_ENV).unwrap_or_else(|_| "bridge".into()),
    );
    std::env::set_var(
        "LOOM_SESSION_API_URL",
        std::env::var(API_ENV).unwrap_or_else(|_| "http://loom:7878".into()),
    );
    std::env::set_var("WEAVER_HOME", root.join(".weaver"));
    std::env::set_var("WEAVER_TAPESTRY_DIR", root.join(".weaver/sock"));
    std::env::set_var(SESSION_ENV, session);
}

fn tapestry_bin() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("tapestry")
}

async fn wait_for_screen(session: &str, markers: &[&str]) -> String {
    let mut screen = String::new();
    for _ in 0..200 {
        screen = loom::backend::capture(session, 0).await.unwrap();
        if markers.iter().all(|marker| screen.contains(marker)) {
            return screen;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("{session} never printed {markers:?}; terminal output:\n{screen}");
}

#[test]
#[serial]
fn docker_runner_child() {
    if std::env::var(CHILD_FLAG).as_deref() != Ok("1") {
        return;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).unwrap());
    let session = std::env::var(SESSION_ENV).unwrap();
    configure(&root, &session);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let options = tapestry::LaunchOptions {
            name: &session,
            cwd: Path::new("/home/app/work"),
            script: "python3 -c 'import os, urllib.request; print(urllib.request.urlopen(os.environ[\"WEAVER_API\"], timeout=5).status)'; printf 'runner-ready owner-host=%s\\n' \"$(cat /etc/hostname)\"; sleep 60",
            env: &[],
            env_clear: false,
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Pty,
            segment_max_bytes: None,
            supervisor_bin: None,
        };
        loom::runner::spawn(&options, 1).await.unwrap();

        let host_session = format!("{session}-host-shell");
        let supervisor_bin = tapestry_bin();
        assert!(
            supervisor_bin.is_file(),
            "tapestry binary missing at {}; run `cargo build -p tapestry` first",
            supervisor_bin.display()
        );
        let host_options = tapestry::LaunchOptions {
            name: &host_session,
            cwd: &root,
            script: "printf 'host-shell-ready host=%s\\n' \"$(cat /etc/hostname)\"; sleep 60",
            env: &[],
            env_clear: false,
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Pty,
            segment_max_bytes: None,
            supervisor_bin: Some(&supervisor_bin),
        };
        loom::runner::spawn_on_host(&host_options).await.unwrap();
    });
}

#[test]
#[ignore = "requires a local Docker daemon and loom image"]
#[serial]
fn docker_runner_supervisor_outlives_its_launcher() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("work")).unwrap();
    std::fs::create_dir_all(root.path().join("uv")).unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    std::fs::set_permissions(
        root.path().join("work"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    std::fs::set_permissions(
        root.path().join("uv"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    let session = format!("docker-runner-test-{}", std::process::id());
    let network = format!("loom-runner-test-{}", std::process::id());
    let api_container = format!("loom-runner-api-test-{}", std::process::id());
    let image = std::env::var("LOOM_DOCKER_TEST_IMAGE").unwrap_or_else(|_| "loom:latest".into());
    assert!(Command::new("docker")
        .args(["network", "create", &network])
        .status()
        .unwrap()
        .success());
    let fixture = DockerFixture {
        api_container: api_container.clone(),
        network: network.clone(),
    };
    assert!(Command::new("docker")
        .args([
            "run",
            "--detach",
            "--name",
            &api_container,
            "--network",
            &network,
            "--network-alias",
            "loom-test-api",
            &image,
            "python3",
            "-m",
            "http.server",
            "7878",
        ])
        .status()
        .unwrap()
        .success());
    std::env::set_var(NETWORK_ENV, &network);
    std::env::set_var(API_ENV, "http://loom-test-api:7878");

    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "docker_runner_child", "--nocapture"])
        .env(CHILD_FLAG, "1")
        .env(ROOT_ENV, root.path())
        .env(SESSION_ENV, &session)
        .status()
        .unwrap();
    assert!(status.success());

    configure(root.path(), &session);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        assert!(tapestry::Client::is_alive(&session).await);
        let owner_screen = wait_for_screen(&session, &["runner-ready", "200"]).await;

        let host_session = format!("{session}-host-shell");
        let host_screen = wait_for_screen(&host_session, &["host-shell-ready"]).await;

        let colocated = format!("{session}-shell");
        let options = tapestry::LaunchOptions {
            name: &colocated,
            cwd: Path::new("/home/app/work"),
            script: "printf 'shell-ready colocated-host=%s\\n' \"$(cat /etc/hostname)\"; sleep 60",
            env: &[],
            env_clear: false,
            cols: 80,
            rows: 24,
            mode: tapestry::Mode::Pty,
            segment_max_bytes: None,
            supervisor_bin: None,
        };
        loom::runner::spawn_colocated(&options, 1, &session)
            .await
            .unwrap();

        let shell_screen = wait_for_screen(&colocated, &["shell-ready"]).await;
        let owner_host = owner_screen
            .split("owner-host=")
            .nth(1)
            .and_then(|tail| tail.split_whitespace().next())
            .expect("owner should print its container hostname");
        let colocated_host = shell_screen
            .split("colocated-host=")
            .nth(1)
            .and_then(|tail| tail.split_whitespace().next())
            .expect("shell should print its container hostname");
        let host_shell_host = host_screen
            .split("host-shell-ready host=")
            .nth(1)
            .and_then(|tail| tail.split_whitespace().next())
            .expect("host shell should print the launcher's hostname");
        assert_eq!(
            colocated_host, owner_host,
            "colocated supervisor should run in the owner's container"
        );
        assert_ne!(
            host_shell_host, owner_host,
            "host supervisor should bypass the session container"
        );

        loom::backend::kill_session_and_wait(&colocated)
            .await
            .unwrap();
        loom::backend::kill_session_and_wait(&host_session)
            .await
            .unwrap();
        loom::backend::kill_session_and_wait(&session)
            .await
            .unwrap();
        assert!(!tapestry::Client::is_alive(&session).await);
    });
    drop(fixture);
}
