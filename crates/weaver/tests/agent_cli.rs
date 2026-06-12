//! Agent-facing CLI flows that must work without a running server.
//!
//! These drive the `weaver` binary directly (`cargo run -- ...`) inside a
//! scratch git repo and assert on stdout and the resulting sqlite rows.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

/// `issue tag` sets a free-form label, `issue show` surfaces it, and `issue
/// untag` clears it.
#[test]
fn issue_tag_set_show_clear() {
    let env = setup();
    run(&env, &["issue", "add", "label", "me"]);

    run(&env, &["issue", "tag", "1", "priority", "high"]);
    let out = run(&env, &["issue", "show", "1"]);
    assert!(out.contains("priority=high"), "show output: {out}");

    // A second set overwrites the value in place (single-valued per key).
    run(&env, &["issue", "tag", "1", "priority", "low"]);
    let out = run(&env, &["issue", "show", "1"]);
    assert!(out.contains("priority=low"), "show output: {out}");
    assert!(!out.contains("priority=high"));

    run(&env, &["issue", "untag", "1", "priority"]);
    let out = run(&env, &["issue", "show", "1"]);
    assert!(!out.contains("priority="), "tag should be cleared: {out}");
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
    let out = run(&env, &["set-status"]);
    assert!(out.contains("open issues: 1"), "status: {out}");
}

/// `issue show` surfaces the live status of the branch working the issue, which
/// is what lets a parent agent poll a delegated sub-tree.
#[test]
fn issue_show_includes_the_working_branch_status() {
    let env = setup();
    run(&env, &["issue", "add", "the", "sub-task"]);
    // The current branch claims it; give the branch a live status.
    run(&env, &["set-status", "blocked", "build", "is", "broken"]);
    let out = run(&env, &["issue", "show", "1"]);
    assert!(
        out.contains("working:"),
        "show should report progress: {out}"
    );
    assert!(
        out.contains("blocked — build is broken"),
        "show should surface the claiming branch's status: {out}"
    );
}

/// `issue wait` returns immediately (success) when the issue is already closed,
/// and reports a closed issue rather than hanging.
#[test]
fn issue_wait_returns_when_already_closed() {
    let env = setup();
    run(&env, &["issue", "add", "done", "already"]);
    run(&env, &["issue", "close", "1"]);
    let out = run(&env, &["issue", "wait", "1", "--timeout", "1"]);
    assert!(
        out.contains("nothing to wait for"),
        "wait on a closed issue should return at once: {out}"
    );
}

/// `issue wait` on a still-open issue gives up at the timeout with a non-zero
/// exit, so a caller can tell "still running" from "finished".
#[test]
fn issue_wait_times_out_on_an_open_issue() {
    let env = setup();
    run(&env, &["issue", "add", "still", "going"]);
    let out = Command::new(weaver_bin())
        .args(["issue", "wait", "1", "--timeout", "1", "--interval", "1"])
        .current_dir(&env.repo_path)
        .env("WEAVER_HOME", &env.home_path)
        .env("WEAVER_API", "http://127.0.0.1:1")
        .output()
        .expect("failed to spawn weaver");
    assert!(
        !out.status.success(),
        "an unmet wait should exit non-zero so callers can branch on it"
    );
    // The timeout is reported through the normal error path (stderr).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("timed out"), "stderr: {stderr}");
}

/// A tracking issue sourced from this branch but claimed by another shows up
/// under "Delegated by this branch", with the sub-agent's status.
#[test]
fn issue_ls_shows_delegated_sub_trees() {
    let env = setup();
    // Simulate a delegation: an issue this branch (feature-test) sourced, now
    // claimed by a child branch with a live status. We seed it straight into
    // the db the same way the launcher would.
    seed_delegated_issue(&env, "feature-test", "weaver/child", "attention", "ready");

    let out = run(&env, &["issue", "ls"]);
    assert!(
        out.contains("Delegated by this branch"),
        "ls should list delegated sub-trees: {out}"
    );
    assert!(out.contains("weaver/child"), "ls: {out}");
    assert!(
        out.contains("attention — ready"),
        "delegated rows show the sub-agent status: {out}"
    );
}

/// Insert a delegated tracking issue (sourced by `parent`, claimed by `child`)
/// and give `child` a branch row with the supplied attention/description —
/// reproducing the state a `loom session launch` from inside `parent` would create.
fn seed_delegated_issue(env: &Env, parent: &str, child: &str, attention: &str, description: &str) {
    let db_path = env.home_path.join("weaver.db");
    // Make sure the parent branch row exists first (a write resolves it).
    run(env, &["goal", "parent", "goal"]);
    let repo_root = canonical_repo_root(&env.repo_path);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = weaver_core::db::connect(&db_path).await.unwrap();
        let child_id = weaver_core::branch::new_id();
        weaver_core::branch::insert(&db, &child_id, &repo_root, child, "main")
            .await
            .unwrap();
        // The attention level lives on the `attention` tag now; `ok` is absence.
        if attention == "ok" {
            weaver_core::tags::clear(&db, &child_id, weaver_core::tags::ATTENTION_KEY)
                .await
                .unwrap();
        } else {
            weaver_core::tags::set(
                &db,
                &child_id,
                weaver_core::tags::ATTENTION_KEY,
                attention,
                "",
                "agent",
            )
            .await
            .unwrap();
        }
        weaver_core::branch::set_description(&db, &child_id, description)
            .await
            .unwrap();
        weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: repo_root.clone(),
                source_branch: Some(parent.to_string()),
                claimed_branch: Some(child.to_string()),
                title: "the delegated task".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    });
}

/// The canonical repo_root weaver keys on (matches `branch::resolve_from_path`).
fn canonical_repo_root(repo: &Path) -> String {
    repo.canonicalize()
        .unwrap_or_else(|_| repo.to_path_buf())
        .display()
        .to_string()
}

/// `summary` is the agent catch-up: it surfaces the goal, the current status,
/// the actual list of outstanding tasks, and a generated next-step hint.
#[test]
fn summary_orients_an_agent_on_the_branch() {
    let env = setup();
    run(&env, &["goal", "ship", "the", "feature"]);
    run(&env, &["issue", "add", "wire", "up", "routes"]);
    run(&env, &["issue", "add", "add", "tests"]);
    run(&env, &["set-status", "ok", "routes", "wired"]);

    let out = run(&env, &["summary"]);
    assert!(out.contains("ship the feature"), "summary: {out}");
    // The current status (level + message) is the where-you-left-off signal.
    assert!(out.contains("ok — routes wired"), "summary: {out}");
    // Outstanding lists the tasks themselves, not just a count.
    assert!(out.contains("Outstanding (2):"), "summary: {out}");
    assert!(out.contains("#1    wire up routes"), "summary: {out}");
    assert!(out.contains("#2    add tests"), "summary: {out}");
    // The next-action hint points at the first open task.
    assert!(out.contains("pick up #1"), "summary: {out}");
    // Every section advertises the command that drills into it.
    for hint in [
        "(weaver goal)",
        "(weaver set-status)",
        "(weaver issue ls)",
        "weaver plan",
        "weaver log",
    ] {
        assert!(out.contains(hint), "summary should surface `{hint}`: {out}");
    }
}

/// The outstanding list is capped (across own issues *and* delegated sub-trees)
/// so a branch with lots of work can't blow up the summary; the overflow
/// collapses into a single "(+N more)" line.
#[test]
fn summary_caps_a_long_outstanding_list() {
    let env = setup();
    for n in 0..13 {
        let title = format!("task{n}");
        run(&env, &["issue", "add", title.as_str()]);
    }
    let out = run(&env, &["summary"]);
    assert!(out.contains("Outstanding (13):"), "summary: {out}");
    // Cap is 10 → the last 3 collapse into one line, not three rows.
    assert!(
        out.contains("(+3 more"),
        "summary should collapse the overflow: {out}"
    );
    assert!(
        !out.contains("task12"),
        "capped tasks should not be printed individually: {out}"
    );
}

/// With nothing open, summary flips its hint to "wrap up / open a PR".
#[test]
fn summary_with_no_open_tasks_suggests_wrapping_up() {
    let env = setup();
    run(&env, &["goal", "tidy", "up"]);
    let out = run(&env, &["summary"]);
    assert!(out.contains("Outstanding: none"), "summary: {out}");
    assert!(out.contains("no open tasks"), "summary: {out}");
    assert!(out.contains("open a PR"), "summary: {out}");
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

/// Run the weaver binary with `stdin` piped in, returning captured stdout. Used
/// to drive the SessionStart hook, which reads its `source` from a JSON payload
/// on stdin.
fn run_with_stdin(env: &Env, args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(weaver_bin())
        .args(args)
        .current_dir(&env.repo_path)
        .env("WEAVER_HOME", &env.home_path)
        .env("WEAVER_API", "http://127.0.0.1:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn weaver");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("weaver did not exit");
    assert!(
        out.status.success(),
        "weaver {args:?} failed: {} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `weaver readme` prints the full weaver workflow guide so an agent can pull
/// the rules back on demand (e.g. after a compaction replayed only the catch-up).
#[test]
fn readme_prints_the_full_weaver_guide() {
    let env = setup();
    let out = run(&env, &["readme"]);
    assert!(
        out.contains("weaver session"),
        "readme should print the WEAVER.md guide: {out}"
    );
    assert!(
        out.contains("weaver set-status"),
        "readme should describe the weaver CLI: {out}"
    );
}

/// On a genuine start/resume/clear (no `compact` source), the session-start hook
/// injects the full WEAVER.md primer as `additionalContext`.
#[test]
fn session_start_hook_injects_the_full_primer() {
    let env = setup();
    let payload = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
    let out = run_with_stdin(&env, &["hook", "--event", "session-start"], payload);
    assert!(
        out.contains("\"hookEventName\":\"SessionStart\""),
        "hook should emit SessionStart additionalContext JSON: {out}"
    );
    // The full guide — not the compact catch-up.
    assert!(
        out.contains("review progress asynchronously"),
        "startup should replay the full WEAVER.md: {out}"
    );
    assert!(
        !out.contains("Context was just compacted"),
        "startup must not use the compaction replay: {out}"
    );
}

/// After a context compaction (`source: "compact"`), the hook replays a concise
/// re-orientation — the `weaver summary` catch-up plus the load-bearing rules and
/// a pointer to `weaver readme` — instead of the whole WEAVER.md.
#[test]
fn session_start_hook_after_compaction_replays_the_concise_summary() {
    let env = setup();
    run(&env, &["goal", "ship", "the", "feature"]);
    run(&env, &["issue", "add", "wire", "up", "routes"]);
    run(&env, &["set-status", "ok", "routes", "wired"]);

    let payload = r#"{"hook_event_name":"SessionStart","source":"compact"}"#;
    let out = run_with_stdin(&env, &["hook", "--event", "session-start"], payload);

    // Still a SessionStart context injection.
    assert!(
        out.contains("\"hookEventName\":\"SessionStart\""),
        "compact replay should still be SessionStart additionalContext: {out}"
    );
    // The concise catch-up: framing, the live branch state, and how-to pointers.
    assert!(
        out.contains("Context was just compacted"),
        "compact replay should re-orient the agent: {out}"
    );
    assert!(
        out.contains("ship the feature"),
        "replay omits the goal: {out}"
    );
    assert!(
        out.contains("ok — routes wired"),
        "replay omits the live status: {out}"
    );
    assert!(
        out.contains("wire up routes"),
        "replay omits the outstanding work: {out}"
    );
    assert!(
        out.contains("weaver readme"),
        "replay should point at the full guide: {out}"
    );
    // It must stay concise — not re-feed the whole WEAVER.md.
    assert!(
        !out.contains("review progress asynchronously"),
        "compact replay must not dump the full WEAVER.md: {out}"
    );

    // The hook still records the lifecycle event (with its source) for the monitor.
    let log = run(&env, &["log"]);
    assert!(
        log.contains("session-start"),
        "the hook should record a session-start event: {log}"
    );
}

#[test]
fn set_status_with_no_id_reports_current_branch() {
    let env = setup();
    run(&env, &["goal", "do", "the", "thing"]);
    run(&env, &["issue", "add", "step", "one"]);
    let out = run(&env, &["set-status"]);
    assert!(out.contains("branch:      feature-test"), "status: {out}");
    assert!(out.contains("goal:        do the thing"), "status: {out}");
    assert!(out.contains("open issues: 1"), "status: {out}");
    // A fresh branch defaults to the calm `ok` attention level.
    assert!(out.contains("status:      ok"), "status: {out}");
}

#[test]
fn set_status_sets_level_and_message() {
    let env = setup();
    // Declare a level with a message, then read it back.
    let out = run(
        &env,
        &[
            "set-status",
            "attention",
            "Waiting",
            "for",
            "PR",
            "feedback",
        ],
    );
    assert!(
        out.contains("attention — Waiting for PR feedback"),
        "set output: {out}"
    );

    let out = run(&env, &["set-status"]);
    assert!(
        out.contains("status:      attention — Waiting for PR feedback"),
        "status read: {out}"
    );

    // A new message replaces the old one.
    run(&env, &["set-status", "ok", "back", "to", "work"]);
    let out = run(&env, &["set-status"]);
    assert!(
        out.contains("status:      ok — back to work"),
        "status read: {out}"
    );

    // A bare level change keeps the last message (the message is the persistent
    // current-state note; only the level is volatile).
    run(&env, &["set-status", "blocked"]);
    let out = run(&env, &["set-status"]);
    assert!(
        out.contains("status:      blocked — back to work"),
        "message should persist across a bare level change: {out}"
    );

    // The set also writes a `tag` event to the branch log (the attention tag).
    let log = run(&env, &["log"]);
    assert!(log.contains("tag"), "log should record tag events: {log}");
    assert!(
        log.contains("attention"),
        "the tag event should carry the attention key: {log}"
    );
}

/// `weaver tag set triage` stamps the overlooker's mark on a *named* session —
/// a status axis distinct from the agent's own `attention` — and records a `tag`
/// event for the audit trail. The agent's attention tag is never touched.
#[test]
fn triage_tag_marks_a_session_without_touching_attention() {
    let env = setup();
    // The agent declares its own attention about itself.
    run(&env, &["set-status", "blocked", "build", "broke"]);

    // No triage tag until an overlooker looks.
    let out = run(&env, &["tag", "ls", "--session", "feature-test"]);
    assert!(
        !out.contains("triage"),
        "fresh session has no triage tag: {out}"
    );

    // An overlooker stamps a *different* opinion on the same session via the
    // triage tag.
    let out = run(
        &env,
        &[
            "tag",
            "set",
            "triage",
            "attention",
            "--note",
            "looks stuck on tests",
            "--by",
            "status-check",
            "--session",
            "feature-test",
        ],
    );
    assert!(out.contains("triage = attention"), "triage tag set: {out}");

    // Read it back with its note and attribution.
    let out = run(&env, &["tag", "ls", "--session", "feature-test"]);
    assert!(out.contains("triage = attention"), "read level: {out}");
    assert!(out.contains("looks stuck on tests"), "read note: {out}");
    assert!(out.contains("status-check"), "read attribution: {out}");

    // The agent's own attention is untouched — two actors, two axes. Its tag
    // sits alongside the triage tag.
    assert!(
        out.contains("attention = blocked"),
        "agent attention must survive a triage write: {out}"
    );
    let out = run(&env, &["set-status"]);
    assert!(
        out.contains("status:      blocked — build broke"),
        "the resolved status reads the agent's attention tag: {out}"
    );

    // The mark is logged as a `tag` event.
    let log = run(&env, &["log"]);
    assert!(log.contains("tag"), "log should record tag events: {log}");

    // Clearing the triage tag leaves the agent's attention untouched.
    run(&env, &["tag", "rm", "triage", "--session", "feature-test"]);
    let out = run(&env, &["tag", "ls", "--session", "feature-test"]);
    assert!(
        !out.contains("triage"),
        "triage tag should be cleared: {out}"
    );
    assert!(
        out.contains("attention = blocked"),
        "clearing triage must not touch attention: {out}"
    );
}

/// A loud key (`attention`/`triage`) rejects a value off the attention ladder
/// with a non-zero exit, like `set-status`.
#[test]
fn tag_set_rejects_invalid_loud_value() {
    let env = setup();
    run(&env, &["goal", "exist"]); // materialize the branch row first
    let out = Command::new(weaver_bin())
        .args(["tag", "set", "triage", "bogus", "--session", "feature-test"])
        .current_dir(&env.repo_path)
        .env("WEAVER_HOME", &env.home_path)
        .env("WEAVER_API", "http://127.0.0.1:1")
        .output()
        .expect("failed to spawn weaver");
    assert!(!out.status.success(), "an invalid loud value should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("attention, blocked"),
        "stderr should name the valid values: {stderr}"
    );
}

/// `weaver tag set/ls/rm` round-trips a free-form (quiet) tag on the current
/// branch with its note and author, and `tag rm` clears it.
#[test]
fn tag_set_ls_rm_roundtrip() {
    let env = setup();
    run(&env, &["goal", "exist"]); // materialize the branch row first

    // No tags to begin with.
    let out = run(&env, &["tag", "ls"]);
    assert!(out.contains("(no tags)"), "fresh branch has no tags: {out}");

    // Set a free-form tag with a note and author.
    let out = run(
        &env,
        &[
            "tag",
            "set",
            "priority",
            "high",
            "--note",
            "ship by friday",
            "--by",
            "russell",
        ],
    );
    assert!(out.contains("priority = high"), "tag set: {out}");

    // List it back with its note and attribution.
    let out = run(&env, &["tag", "ls"]);
    assert!(out.contains("priority = high"), "tag ls value: {out}");
    assert!(out.contains("by russell"), "tag ls author: {out}");
    assert!(out.contains("ship by friday"), "tag ls note: {out}");

    // Setting the same key again overwrites it (single-valued).
    run(&env, &["tag", "set", "priority", "low"]);
    let out = run(&env, &["tag", "ls"]);
    assert!(out.contains("priority = low"), "tag overwrite: {out}");
    assert_eq!(out.matches("priority").count(), 1, "single-valued: {out}");

    // Remove it.
    run(&env, &["tag", "rm", "priority"]);
    let out = run(&env, &["tag", "ls"]);
    assert!(out.contains("(no tags)"), "tag rm cleared it: {out}");
}

/// `set-status ok` clears the agent's `attention` tag (returns to calm) while
/// leaving the branch `description` in place.
#[test]
fn set_status_ok_clears_attention_tag_but_keeps_description() {
    let env = setup();
    // Raise attention with a message.
    run(&env, &["set-status", "attention", "ready", "for", "review"]);
    let out = run(&env, &["tag", "ls"]);
    assert!(
        out.contains("attention = attention"),
        "set-status should write the attention tag: {out}"
    );

    // Return to calm — the attention tag is cleared, the description survives.
    run(&env, &["set-status", "ok"]);
    let out = run(&env, &["tag", "ls"]);
    assert!(
        !out.contains("attention ="),
        "set-status ok should clear the attention tag: {out}"
    );
    let out = run(&env, &["set-status"]);
    assert!(
        out.contains("status:      ok — ready for review"),
        "ok must keep the last description beside the calm level: {out}"
    );
}

#[test]
fn set_status_rejects_unknown_level() {
    let env = setup();
    let out = Command::new(weaver_bin())
        .args(["set-status", "bogus"])
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
