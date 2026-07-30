//! Per-repo setup execution — running a registered repo's committed
//! `.weaver/config.toml` `[setup]` script in a session worktree before the agent
//! starts (install dependencies, prime caches, generate config).
//!
//! **PRIVILEGED.** A setup script is arbitrary repo code that, in loom's single
//! shared container (the design's Model 1), runs with the same credentials and
//! `$HOME` as every other session — it can read every secret loom holds.
//! It is therefore:
//!
//! * gated to **allowlisted (registered) repos only** — the caller checks
//!   [`crate::repo::is_allowlisted`] before running anything here;
//! * bounded by a **timeout** — an overrun is killed (whole process group), so a
//!   hung or runaway bootstrap cannot wedge the launch;
//! * **visibly surfaced** — output is captured to a `setup.log` and the outcome
//!   is recorded as session events; a failure leaves the session in an error
//!   state rather than silently launching a half-provisioned worktree.
//!
//! See the shared-loom design §6.4. This module only *runs* the script; the
//! allowlist gate, env layering, and session-state handling live in the web
//! layer's create path.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// What running a setup script produced.
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    /// True when the script exited 0 within the timeout.
    pub success: bool,
    /// True when the script was killed for overrunning the timeout.
    pub timed_out: bool,
    /// The process exit code, if it exited on its own (`None` on timeout, or when
    /// it was killed by a signal).
    pub exit_code: Option<i32>,
    /// Combined stdout+stderr, in the order produced (the script runs with
    /// `exec 2>&1`). Captured even on timeout (the output up to the kill).
    pub output: String,
    /// How long the script ran before completing or being killed.
    pub duration: Duration,
}

impl SetupOutcome {
    /// A one-line human summary for an event payload / log line.
    pub fn summary(&self) -> String {
        if self.success {
            format!("setup succeeded in {:.1}s", self.duration.as_secs_f64())
        } else if self.timed_out {
            format!(
                "setup timed out after {:.0}s and was killed",
                self.duration.as_secs_f64()
            )
        } else {
            match self.exit_code {
                Some(code) => format!("setup failed (exit {code})"),
                None => "setup failed (killed by signal)".to_string(),
            }
        }
    }
}

/// Run `script` in `work_dir` with `env` overlaid on the inherited environment,
/// bounded by `timeout`. The script is wrapped so it runs fail-fast
/// (`set -e`) with stderr merged into stdout (`exec 2>&1`); combined output is
/// captured and, when `log_path` is given, streamed to that file as it arrives
/// so it can be tailed live and inspected after.
///
/// The child is its own process group leader, so a timeout kills the whole tree
/// (a bootstrap that spawns `npm`, `cargo`, … leaves nothing behind). Returns the
/// [`SetupOutcome`]; an `Err` is only the rare case of failing to *spawn* a shell
/// at all.
pub async fn run(
    work_dir: &Path,
    script: &str,
    env: &[(String, String)],
    timeout: Duration,
    log_path: Option<&Path>,
) -> std::io::Result<SetupOutcome> {
    // Merge stderr→stdout (ordered capture) and fail fast on the first error.
    let wrapped = format!("exec 2>&1\nset -e\n{script}");

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&wrapped)
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Own process group so a timeout can SIGKILL the whole tree, not just sh.
        .process_group(0);
    for (key, value) in env {
        cmd.env(key, value);
    }

    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");

    // Drain the pipe in a task while we await the child, so a chatty script can't
    // deadlock on a full pipe buffer. The task owns the (optional) log file and
    // returns the full captured output — including whatever arrived before a kill.
    let log_path = log_path.map(Path::to_path_buf);
    let reader = tokio::spawn(async move {
        let mut log = match log_path {
            Some(path) => tokio::fs::File::create(&path).await.ok(),
            None => None,
        };
        let mut buf = String::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            buf.push_str(&line);
            buf.push('\n');
            if let Some(file) = log.as_mut() {
                let _ = file.write_all(line.as_bytes()).await;
                let _ = file.write_all(b"\n").await;
                let _ = file.flush().await;
            }
        }
        buf
    });

    let (timed_out, exit_code) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (false, status.code()),
        // `wait()` itself errored (very unusual) — treat as a failed run.
        Ok(Err(_)) => (false, None),
        Err(_elapsed) => {
            kill_group(&child);
            let _ = child.wait().await;
            (true, None)
        }
    };

    let output = reader.await.unwrap_or_default();
    let success = !timed_out && exit_code == Some(0);
    Ok(SetupOutcome {
        success,
        timed_out,
        exit_code,
        output,
        duration: start.elapsed(),
    })
}

/// SIGKILL the child's whole process group. The child was spawned as a group
/// leader (`process_group(0)`), so its pgid equals its pid and a negative pid
/// targets every process in the group.
#[cfg(unix)]
fn kill_group(child: &tokio::process::Child) {
    if let Some(pid) = child.id() {
        // SAFETY: a plain `kill(2)` on our own child's process group; the pgid
        // equals the child pid because it was spawned with `process_group(0)`.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_child: &tokio::process::Child) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(String, String)> {
        Vec::new()
    }

    #[tokio::test]
    async fn captures_output_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            dir.path(),
            "echo hello; echo world",
            &env(),
            Duration::from_secs(30),
            None,
        )
        .await
        .unwrap();
        assert!(out.success, "{}", out.summary());
        assert_eq!(out.exit_code, Some(0));
        assert!(out.output.contains("hello"));
        assert!(out.output.contains("world"));
    }

    #[tokio::test]
    async fn merges_stderr_and_runs_in_work_dir_with_env() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            dir.path(),
            "echo to-stderr 1>&2; pwd; echo \"$MY_VAR\"",
            &[("MY_VAR".to_string(), "from-env".to_string())],
            Duration::from_secs(30),
            None,
        )
        .await
        .unwrap();
        assert!(out.success, "{}", out.summary());
        // stderr was merged into the captured output…
        assert!(out.output.contains("to-stderr"), "output: {}", out.output);
        // …the cwd is the work dir…
        let canonical = dir.path().canonicalize().unwrap();
        assert!(
            out.output.contains(&canonical.display().to_string()),
            "pwd should be the work dir; output: {}",
            out.output
        );
        // …and the overlaid env var is visible.
        assert!(out.output.contains("from-env"), "output: {}", out.output);
    }

    #[tokio::test]
    async fn fails_fast_and_reports_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        // `set -e` aborts after the failing line; the marker must not appear.
        let out = run(
            dir.path(),
            "echo before; exit 7; echo after",
            &env(),
            Duration::from_secs(30),
            None,
        )
        .await
        .unwrap();
        assert!(!out.success);
        assert!(!out.timed_out);
        assert_eq!(out.exit_code, Some(7));
        assert!(out.output.contains("before"));
        assert!(!out.output.contains("after"));
        assert!(out.summary().contains("exit 7"));
    }

    #[tokio::test]
    async fn times_out_and_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            dir.path(),
            "echo starting; sleep 30",
            &env(),
            Duration::from_millis(300),
            None,
        )
        .await
        .unwrap();
        assert!(out.timed_out, "{}", out.summary());
        assert!(!out.success);
        // Output captured up to the kill is still returned.
        assert!(out.output.contains("starting"));
        // We didn't wait the full 30s.
        assert!(out.duration < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn writes_the_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("setup.log");
        let out = run(
            dir.path(),
            "echo logged-line",
            &env(),
            Duration::from_secs(30),
            Some(&log),
        )
        .await
        .unwrap();
        assert!(out.success);
        let contents = std::fs::read_to_string(&log).unwrap();
        assert!(contents.contains("logged-line"), "log: {contents}");
    }
}
