//! Agent-facing CLI flows that must work without a running server.
//!
//! These drive the `weaver` binary directly (`cargo run -- ...`) inside a
//! scratch git repo and assert on stdout and the resulting sqlite rows.

use std::path::Path;
use std::process::Command;

fn sh(dir: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    assert!(status.success(), "{program} {args:?} failed");
}

/// Path to the freshly-built `weaver` binary the test will drive.
fn weaver_bin() -> std::path::PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for integration tests.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_weaver"))
}

struct Env {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    repo_path: std::path::PathBuf,
    home_path: std::path::PathBuf,
}

fn setup() -> Env {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    sh(repo.path(), "git", &["init", "-b", "feature-test"]);
    sh(repo.path(), "git", &["config", "user.email", "t@t.test"]);
    sh(repo.path(), "git", &["config", "user.name", "Test"]);
    std::fs::write(repo.path().join("README.md"), "hi").unwrap();
    sh(repo.path(), "git", &["add", "."]);
    sh(repo.path(), "git", &["commit", "-m", "init"]);
    let repo_path = repo.path().to_path_buf();
    let home_path = home.path().to_path_buf();
    Env {
        _home: home,
        _repo: repo,
        repo_path,
        home_path,
    }
}

/// Run the weaver binary in `dir` with `WEAVER_HOME=<home>` and the given args,
/// returning the captured stdout.
fn run(env: &Env, args: &[&str]) -> String {
    let out = Command::new(weaver_bin())
        .args(args)
        .current_dir(&env.repo_path)
        .env("WEAVER_HOME", &env.home_path)
        // Make sure the CLI can't accidentally reach a real server.
        .env("WEAVER_API", "http://127.0.0.1:1")
        .output()
        .expect("failed to spawn weaver");
    assert!(
        out.status.success(),
        "weaver {args:?} failed: {} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn goal_set_and_get() {
    let env = setup();
    // No goal yet — prints an empty line.
    let out = run(&env, &["goal"]);
    assert_eq!(out.trim(), "");

    run(&env, &["goal", "ship", "the", "thing"]);
    let out = run(&env, &["goal"]);
    assert_eq!(out.trim(), "ship the thing");
}

#[test]
fn where_reports_resolved_branch() {
    let env = setup();
    let out = run(&env, &["where"]);
    assert!(
        out.contains("branch:    feature-test"),
        "where output: {out}"
    );
}

#[test]
fn issue_lifecycle() {
    let env = setup();
    run(&env, &["issue", "add", "fix", "the", "thing"]);
    run(&env, &["issue", "add", "another", "task"]);

    let out = run(&env, &["issue", "ls"]);
    assert!(out.contains("fix the thing"), "ls output: {out}");
    assert!(out.contains("another task"), "ls output: {out}");
    assert_eq!(out.matches("[ ]").count(), 2, "two open issues");

    // Close #1, then list defaults to open only (should drop it).
    run(&env, &["issue", "close", "1"]);
    let out = run(&env, &["issue", "ls"]);
    assert!(
        !out.contains("fix the thing"),
        "closed issue should be hidden"
    );

    let out = run(&env, &["issue", "ls", "--all"]);
    assert!(
        out.contains("[x]"),
        "closed marker should appear with --all"
    );

    // Reopen, then rm.
    run(&env, &["issue", "reopen", "1"]);
    let out = run(&env, &["issue", "ls"]);
    assert_eq!(out.matches("[ ]").count(), 2);

    run(&env, &["issue", "rm", "1"]);
    let out = run(&env, &["issue", "ls", "--all"]);
    assert!(!out.contains("fix the thing"));
}

#[test]
fn issue_ls_separates_branch_work_from_repo_backlog() {
    let env = setup();
    // Default add → claimed by this branch. `--repo` → unclaimed backlog.
    run(&env, &["issue", "add", "my", "task"]);
    run(&env, &["issue", "add", "--repo", "backlog", "task"]);

    // Default ls shows both, under separate sections.
    let out = run(&env, &["issue", "ls"]);
    assert!(out.contains("On this branch"), "ls: {out}");
    assert!(out.contains("my task"), "ls: {out}");
    assert!(out.contains("Repo backlog"), "ls: {out}");
    assert!(out.contains("backlog task"), "ls: {out}");

    // `--mine` drops the backlog section.
    let out = run(&env, &["issue", "ls", "--mine"]);
    assert!(out.contains("my task"), "mine: {out}");
    assert!(
        !out.contains("backlog task"),
        "mine should hide backlog: {out}"
    );

    // The badge counts only this branch's claimed work, not the backlog.
    let out = run(&env, &["status"]);
    assert!(out.contains("open issues: 1"), "status: {out}");
}

#[test]
fn note_writes_an_event() {
    let env = setup();
    run(&env, &["note", "made", "a", "decision"]);
    let log = run(&env, &["log"]);
    assert!(log.contains("made a decision"), "log: {log}");
}

#[test]
fn hook_writes_an_event_row() {
    let env = setup();
    run(&env, &["hook", "--event", "working"]);
    let log = run(&env, &["log"]);
    assert!(
        log.contains("hook"),
        "log should mention the hook event: {log}"
    );
    assert!(
        log.contains("working"),
        "log should mention the event name: {log}"
    );
}

#[test]
fn status_with_no_id_reports_current_branch() {
    let env = setup();
    run(&env, &["goal", "do", "the", "thing"]);
    run(&env, &["issue", "add", "step", "one"]);
    let out = run(&env, &["status"]);
    assert!(out.contains("branch:      feature-test"), "status: {out}");
    assert!(out.contains("goal:        do the thing"), "status: {out}");
    assert!(out.contains("open issues: 1"), "status: {out}");
    // A fresh branch defaults to the calm `ok` attention level.
    assert!(out.contains("attention:   ok"), "status: {out}");
}

#[test]
fn status_set_attention_level_and_note() {
    let env = setup();
    // Declare an attention level with a note, then read it back.
    let out = run(
        &env,
        &["status", "attention", "Waiting", "for", "PR", "feedback"],
    );
    assert!(
        out.contains("attention — Waiting for PR feedback"),
        "set output: {out}"
    );

    let out = run(&env, &["status"]);
    assert!(
        out.contains("attention:   attention — Waiting for PR feedback"),
        "status read: {out}"
    );

    // Setting a level with no note clears the note.
    run(&env, &["status", "ok"]);
    let out = run(&env, &["status"]);
    assert!(out.contains("attention:   ok"), "status read: {out}");
    assert!(
        !out.contains("Waiting for PR feedback"),
        "note should be cleared: {out}"
    );

    // The set also writes an `attention` event to the branch log.
    let log = run(&env, &["log"]);
    assert!(
        log.contains("attention"),
        "log should record attention events: {log}"
    );
}

#[test]
fn status_set_rejects_unknown_level() {
    let env = setup();
    let out = Command::new(weaver_bin())
        .args(["status", "bogus"])
        .current_dir(&env.repo_path)
        .env("WEAVER_HOME", &env.home_path)
        .env("WEAVER_API", "http://127.0.0.1:1")
        .output()
        .expect("failed to spawn weaver");
    assert!(!out.status.success(), "unknown level should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown status 'bogus'"),
        "stderr: {stderr}"
    );
}

#[test]
fn auto_creates_branch_row_on_first_write() {
    let env = setup();
    // The branch row should not exist before any write.
    let db = env.home_path.join("weaver.db");
    assert!(
        !db.exists()
            || std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0) == 0
            || count_branches(&db) == 0
    );
    run(&env, &["goal", "first", "write"]);
    assert_eq!(count_branches(&db), 1);
}

fn count_branches(db_path: &Path) -> i64 {
    // Use a tiny tokio runtime to query the DB synchronously here.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = weaver_core::db::connect(db_path).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM branches")
            .fetch_one(&db)
            .await
            .unwrap();
        n
    })
}
