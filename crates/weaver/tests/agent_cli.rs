//! Agent-facing CLI flows against a real (locally isolated) loom server — the
//! CLI's only mode now that it is an HTTP-only client of loom (see
//! `weaver_api::endpoint`). Each test boots its own server on a random port
//! with an isolated `WEAVER_HOME`, seeds one branch row, and drives the
//! `weaver` binary as a subprocess pointed at it via `$WEAVER_API`/
//! `$WEAVER_BRANCH` — the same env loom injects into every session it
//! launches.
//!
//! `Env::start` mutates process-global env (`WEAVER_HOME`), so every test is
//! `#[serial]` — they share one binary and would otherwise race on that env.

use std::io::Write;
use std::net::SocketAddr;
use std::process::{Command, Stdio};

use loom::events::EventBus;
use loom::web::AppState;
use loom::{db, server};
use serial_test::serial;
use tokio::net::TcpListener;

/// Path to the freshly-built `weaver` binary the test will drive.
fn weaver_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_weaver"))
}

/// A running loom server, isolated in its own temp `WEAVER_HOME`/sqlite db,
/// with one branch row seeded — the target `$WEAVER_BRANCH` names, exactly as
/// loom would set it for a real session.
struct Env {
    addr: SocketAddr,
    branch_id: String,
    repo_root: String,
    branch_name: String,
    db: weaver_core::db::Db,
    _home: tempfile::TempDir,
}

impl Env {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        // `seed_owner` no longer defaults to a real login — this suite's
        // requests (the `weaver` CLI, over loopback) need a seeded owner to
        // resolve to.
        std::env::set_var("LOOM_OWNER_GITHUB", "rjpower");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pool = db::connect(&db::default_db_path()).await.unwrap();

        let repo_root = "/repo".to_string();
        let branch_name = "feature-test".to_string();
        let branch = weaver_core::branch::upsert(&pool, &repo_root, &branch_name, "main")
            .await
            .unwrap();

        let trigger = loom::github_trigger::GithubTrigger::production(pool.clone());
        let state = AppState {
            db: pool.clone(),
            bus: EventBus::new(),
            addr: addr.to_string(),
            ide: std::sync::Arc::new(loom::ide::IdeManager::new(loom::ide::ide_home())),
            trigger,
            acp: loom::acp::AcpRegistry::new(),
            launch_gate: loom::launch_gate::RepoLaunchGate::default(),
        };
        tokio::spawn(server::serve(state, listener));

        let env = Env {
            addr,
            branch_id: branch.id,
            repo_root,
            branch_name,
            db: pool,
            _home: home,
        };
        env.wait_until_healthy().await;
        env
    }

    async fn wait_until_healthy(&self) {
        let url = format!("http://{}/api/health", self.addr);
        for _ in 0..100 {
            if reqwest::get(&url).await.is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("server never became healthy at {url}");
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(weaver_bin());
        cmd.args(args)
            .env("WEAVER_API", format!("http://{}", self.addr))
            .env("WEAVER_BRANCH", &self.branch_id)
            .env_remove("LOOM_TOKEN");
        cmd
    }

    /// Run the weaver binary with the given args, returning captured stdout.
    /// Asserts success.
    fn run(&self, args: &[&str]) -> String {
        let out = self.command(args).output().expect("failed to spawn weaver");
        assert!(
            out.status.success(),
            "weaver {args:?} failed: {} / {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run the weaver binary with `stdin` piped in, returning captured stdout.
    /// Used to drive the SessionStart hook, which reads its `source` from a
    /// JSON payload on stdin.
    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> String {
        let mut child = self
            .command(args)
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

    /// Run the weaver binary and return the raw output (not asserting on
    /// success) — for tests exercising a failure path.
    fn run_raw(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().expect("failed to spawn weaver")
    }
}

/// The goal lives as the `goal` artifact; writing it keeps the branch's
/// denormalized goal (what `weaver status` reads back) in sync.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn goal_artifact_write_syncs_the_branch_goal() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifact", "write", "goal"], "ship the thing\n");
    let out = env.run(&["artifact", "show", "goal"]);
    assert_eq!(out.trim(), "ship the thing");
    let out = env.run(&["status"]);
    assert!(out.contains("goal:        ship the thing"), "status: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn where_reports_resolved_branch() {
    let env = Env::start().await;
    let out = env.run(&["where"]);
    assert!(
        out.contains("branch:    feature-test"),
        "where output: {out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_weaver_branch_gives_a_friendly_error() {
    let env = Env::start().await;
    let out = std::process::Command::new(weaver_bin())
        .args(["where"])
        .env("WEAVER_API", format!("http://{}", env.addr))
        .env_remove("WEAVER_BRANCH")
        .env_remove("LOOM_TOKEN")
        .output()
        .expect("failed to spawn weaver");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("WEAVER_BRANCH"),
        "should name the missing env var: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unreachable_loom_gives_a_friendly_error() {
    let out = std::process::Command::new(weaver_bin())
        .args(["where"])
        // Port 1 is (almost) never listening — a fast, reliable connection
        // refusal without a real unreachable-network dependency.
        .env("WEAVER_API", "http://127.0.0.1:1")
        .env("WEAVER_BRANCH", "does-not-matter")
        .env_remove("LOOM_TOKEN")
        .output()
        .expect("failed to spawn weaver");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot reach loom"),
        "stderr should give a friendly connection error: {stderr}"
    );
    assert!(
        stderr.contains("loom server start"),
        "stderr should say how to fix it: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_lifecycle() {
    let env = Env::start().await;
    env.run(&["issue", "add", "fix", "the", "thing"]);
    env.run(&["issue", "add", "another", "task"]);

    let out = env.run(&["issue", "ls"]);
    assert!(out.contains("fix the thing"), "ls output: {out}");
    assert!(out.contains("another task"), "ls output: {out}");
    assert_eq!(out.matches("[ ]").count(), 2, "two open issues");

    // Close #1, then list defaults to open only (should drop it).
    env.run(&["issue", "close", "1"]);
    let out = env.run(&["issue", "ls"]);
    assert!(
        !out.contains("fix the thing"),
        "closed issue should be hidden"
    );

    let out = env.run(&["issue", "ls", "--all"]);
    assert!(
        out.contains("[x]"),
        "closed marker should appear with --all"
    );

    // Reopen, then rm.
    env.run(&["issue", "reopen", "1"]);
    let out = env.run(&["issue", "ls"]);
    assert_eq!(out.matches("[ ]").count(), 2);

    env.run(&["issue", "rm", "1"]);
    let out = env.run(&["issue", "ls", "--all"]);
    assert!(!out.contains("fix the thing"));
}

/// `issue tag set` sets a free-form label, `issue show` surfaces it, and
/// `issue tag rm` clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_tag_set_show_clear() {
    let env = Env::start().await;
    env.run(&["issue", "add", "label", "me"]);

    env.run(&["issue", "tag", "set", "1", "priority", "high"]);
    let out = env.run(&["issue", "show", "1"]);
    assert!(out.contains("priority=high"), "show output: {out}");

    // A second set overwrites the value in place (single-valued per key).
    env.run(&["issue", "tag", "set", "1", "priority", "low"]);
    let out = env.run(&["issue", "show", "1"]);
    assert!(out.contains("priority=low"), "show output: {out}");
    assert!(!out.contains("priority=high"));

    env.run(&["issue", "tag", "rm", "1", "priority"]);
    let out = env.run(&["issue", "show", "1"]);
    assert!(!out.contains("priority="), "tag should be cleared: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_ls_separates_branch_work_from_repo_backlog() {
    let env = Env::start().await;
    // Default add → claimed by this branch. `--repo` → unclaimed backlog.
    env.run(&["issue", "add", "my", "task"]);
    env.run(&["issue", "add", "--repo", "backlog", "task"]);

    // Default ls shows both, under separate sections.
    let out = env.run(&["issue", "ls"]);
    assert!(out.contains("On this branch"), "ls: {out}");
    assert!(out.contains("my task"), "ls: {out}");
    assert!(out.contains("Repo backlog"), "ls: {out}");
    assert!(out.contains("backlog task"), "ls: {out}");

    // `--mine` drops the backlog section.
    let out = env.run(&["issue", "ls", "--mine"]);
    assert!(out.contains("my task"), "mine: {out}");
    assert!(
        !out.contains("backlog task"),
        "mine should hide backlog: {out}"
    );

    // The badge counts only this branch's claimed work, not the backlog.
    let out = env.run(&["status"]);
    assert!(out.contains("open issues: 1"), "status: {out}");
}

/// `issue show` surfaces the live status of the branch working the issue, which
/// is what lets a parent agent poll a delegated sub-tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_show_includes_the_working_branch_status() {
    let env = Env::start().await;
    env.run(&["issue", "add", "the", "sub-task"]);
    // The current branch claims it; give the branch a live status.
    env.run(&["status", "blocked", "build", "is", "broken"]);
    let out = env.run(&["issue", "show", "1"]);
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_wait_returns_when_already_closed() {
    let env = Env::start().await;
    env.run(&["issue", "add", "done", "already"]);
    env.run(&["issue", "close", "1"]);
    let out = env.run(&["issue", "wait", "1", "--timeout", "1"]);
    assert!(
        out.contains("nothing to wait for"),
        "wait on a closed issue should return at once: {out}"
    );
}

/// `issue wait` on a still-open issue gives up at the timeout with a non-zero
/// exit, so a caller can tell "still running" from "finished".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_wait_times_out_on_an_open_issue() {
    let env = Env::start().await;
    env.run(&["issue", "add", "still", "going"]);
    let out = env.run_raw(&["issue", "wait", "1", "--timeout", "1", "--interval", "1"]);
    assert!(
        !out.status.success(),
        "an unmet wait should exit non-zero so callers can branch on it"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("timed out"), "stderr: {stderr}");
}

/// A tracking issue sourced from this branch but claimed by another shows up
/// under "Delegated by this branch", with the sub-agent's status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_ls_shows_delegated_sub_trees() {
    let env = Env::start().await;
    seed_delegated_issue(&env, "weaver/child", "attention", "ready").await;

    let out = env.run(&["issue", "ls"]);
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

/// Insert a delegated tracking issue (sourced by the env's branch, claimed by
/// `child`) and give `child` a branch row with the supplied attention/
/// description — reproducing the state a `loom session launch` from inside
/// the parent would create.
async fn seed_delegated_issue(env: &Env, child: &str, attention: &str, description: &str) {
    let child_id = weaver_core::branch::new_id();
    weaver_core::branch::insert(&env.db, &child_id, &env.repo_root, child, "main")
        .await
        .unwrap();
    // The attention level lives on the `attention` tag now; `ok` is absence.
    if attention == "ok" {
        weaver_core::tags::clear(&env.db, &child_id, weaver_core::tags::ATTENTION_KEY)
            .await
            .unwrap();
    } else {
        weaver_core::tags::set(
            &env.db,
            &child_id,
            weaver_core::tags::ATTENTION_KEY,
            attention,
            "",
            "agent",
        )
        .await
        .unwrap();
    }
    weaver_core::branch::set_description(&env.db, &child_id, description)
        .await
        .unwrap();
    weaver_core::issue::add(
        &env.db,
        &weaver_core::issue::NewIssue {
            repo_root: env.repo_root.clone(),
            source_branch: Some(env.branch_name.clone()),
            claimed_branch: Some(child.to_string()),
            title: "the delegated task".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

/// `summary` is the agent catch-up: it surfaces the goal, the current status,
/// the actual list of outstanding tasks, and a generated next-step hint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn summary_orients_an_agent_on_the_branch() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifact", "write", "goal"], "ship the feature\n");
    env.run(&["issue", "add", "wire", "up", "routes"]);
    env.run(&["issue", "add", "add", "tests"]);
    env.run(&["status", "ok", "routes", "wired"]);

    let out = env.run(&["summary"]);
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
        "(weaver artifact show goal)",
        "(weaver status)",
        "(weaver issue ls)",
        "weaver artifact",
        "weaver log",
    ] {
        assert!(out.contains(hint), "summary should surface `{hint}`: {out}");
    }
}

/// The outstanding list is capped (across own issues *and* delegated sub-trees)
/// so a branch with lots of work can't blow up the summary; the overflow
/// collapses into a single "(+N more)" line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn summary_caps_a_long_outstanding_list() {
    let env = Env::start().await;
    for n in 0..13 {
        let title = format!("task{n}");
        env.run(&["issue", "add", title.as_str()]);
    }
    let out = env.run(&["summary"]);
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn summary_with_no_open_tasks_suggests_wrapping_up() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifact", "write", "goal"], "tidy up\n");
    let out = env.run(&["summary"]);
    assert!(out.contains("Outstanding: none"), "summary: {out}");
    assert!(out.contains("no open tasks"), "summary: {out}");
    assert!(out.contains("open a PR"), "summary: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_write_show_ls_and_revisions() {
    let env = Env::start().await;
    // Write from stdin (no file arg). The CLI prints a dashboard URL (now
    // always known — the write only succeeds because loom is reachable) and
    // the new revision.
    let out = env.run_with_stdin(&["artifact", "write", "plan"], "# Plan\n\nDesign here.\n");
    assert!(
        out.contains("/artifacts/plan"),
        "write should print the URL: {out}"
    );
    assert!(out.contains("rev 1"), "first write is rev 1: {out}");

    // show prints the content verbatim.
    let shown = env.run(&["artifact", "show", "plan"]);
    assert!(shown.contains("Design here."), "show: {shown}");

    // A second write appends rev 2, and --rev 1 still fetches the original.
    let out2 = env.run_with_stdin(&["artifact", "write", "plan"], "# Plan v2\n");
    assert!(out2.contains("rev 2"), "second write is rev 2: {out2}");
    let v1 = env.run(&["artifact", "show", "plan", "--rev", "1"]);
    assert!(v1.contains("Design here."), "rev 1 is the original: {v1}");

    // --meta prints the envelope, not the content.
    let meta = env.run(&["artifact", "show", "plan", "--meta"]);
    assert!(meta.contains("name:    plan"), "meta: {meta}");
    assert!(meta.contains("rev:     2"), "meta latest rev: {meta}");
    assert!(
        meta.contains("branch"),
        "meta scope is branch-scoped: {meta}"
    );

    // ls lists the branch-scoped artifact.
    let ls = env.run(&["artifact", "ls"]);
    assert!(ls.contains("plan"), "ls: {ls}");
    assert!(ls.contains("rev 2"), "ls shows latest rev: {ls}");

    // A --repo write is repo-shared; --repo ls shows it.
    env.run_with_stdin(&["artifact", "write", "shared", "--repo"], "shared body\n");
    let repo_ls = env.run(&["artifact", "ls", "--repo"]);
    assert!(
        repo_ls.contains("repo:shared"),
        "repo ls shows shared scope: {repo_ls}"
    );

    // rm reports the scope and revision it removed, then the artifact is gone.
    let rm = env.run(&["artifact", "rm", "plan"]);
    assert!(rm.contains("was rev 2"), "rm: {rm}");
    let ls = env.run(&["artifact", "ls"]);
    assert!(!ls.contains("plan"), "rm should remove it: {ls}");
}

/// The URL printed after a write is resolved server-side, so it carries the
/// operator's externally-visible origin — not the loopback/wildcard address the
/// agent dialed (`http://0.0.0.0:7878`), which is useless to whoever reads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_write_url_honours_the_public_base() {
    let env = Env::start().await;

    // With no `auth.base_url`, the origin is derived from the request's Host —
    // here the loopback address the CLI dialed, right for a single-machine loom.
    let derived = env.run_with_stdin(&["artifact", "write", "plan"], "# Plan\n");
    assert!(
        derived.contains(&format!(
            "http://{}/s/{}/artifacts/plan",
            env.addr, env.branch_id
        )),
        "derived from the request origin, keyed off $WEAVER_BRANCH: {derived}"
    );

    // Once the operator declares a public origin, the printed link is one an
    // off-box reader (of a PR, say) can actually open — and the dialed address
    // no longer leaks into it.
    loom::config::apply(
        &env.db,
        &[(
            "auth.base_url".to_string(),
            Some("https://loom.example.com".to_string()),
        )],
    )
    .await
    .unwrap();
    let public = env.run_with_stdin(&["artifact", "write", "plan"], "# Plan v2\n");
    assert!(
        public.contains(&format!(
            "https://loom.example.com/s/{}/artifacts/plan  (rev 2, this branch)",
            env.branch_id
        )),
        "the configured public origin wins: {public}"
    );
    assert!(
        !public.contains(&env.addr.to_string()),
        "the dialed address is not leaked into the link: {public}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_comment_thread_and_resolve_roundtrip() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifact", "write", "plan"], "# Plan\n\nDesign here.\n");

    // No thread yet.
    let threads = env.run(&["artifact", "threads", "plan"]);
    assert!(
        threads.contains("no open threads"),
        "no threads yet: {threads}"
    );

    // Opening a thread requires --quote.
    let missing_quote = env.run_raw(&["artifact", "comment", "plan", "looks off"]);
    assert!(
        !missing_quote.status.success(),
        "comment without --quote or --thread should fail"
    );

    // Open a new thread anchored to a quote, seeded with its first comment.
    let opened = env.run(&[
        "artifact",
        "comment",
        "plan",
        "--quote",
        "Design here.",
        "this needs more detail",
    ]);
    assert!(opened.contains("opened thread"), "opened: {opened}");

    let threads = env.run(&["artifact", "threads", "plan"]);
    assert!(threads.contains("this needs more detail"), "{threads}");
    assert!(threads.contains("agent:"), "author is agent: {threads}");

    // Extract the thread id printed by `comment`, then reply and resolve it.
    let tid: i64 = opened
        .split_whitespace()
        .nth(2)
        .expect("opened thread <id>")
        .parse()
        .expect("thread id is numeric");

    let replied = env.run(&[
        "artifact",
        "comment",
        "plan",
        "--thread",
        &tid.to_string(),
        "fixed, take a look",
    ]);
    assert!(replied.contains("added comment"), "replied: {replied}");

    let threads = env.run(&["artifact", "threads", "plan"]);
    assert!(threads.contains("fixed, take a look"), "{threads}");

    env.run(&["artifact", "resolve", "plan", &tid.to_string()]);
    let threads = env.run(&["artifact", "threads", "plan"]);
    assert!(
        threads.contains("no open threads"),
        "resolved thread should no longer be open: {threads}"
    );
    let all = env.run(&["artifact", "threads", "plan", "--all"]);
    assert!(
        all.contains("this needs more detail"),
        "--all still shows the resolved thread: {all}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn hook_writes_an_event_row() {
    let env = Env::start().await;
    env.run(&["hook", "--event", "working"]);
    let log = env.run(&["log"]);
    assert!(
        log.contains("hook"),
        "log should mention the hook event: {log}"
    );
    assert!(
        log.contains("working"),
        "log should mention the event name: {log}"
    );
}

/// A nested, isolated agent (a headless `claude -p` review/lint/one-shot) still
/// fires the worktree's weaver lifecycle hooks, but the spawner strips
/// `$WEAVER_BRANCH` so the child cannot impersonate the parent. With no branch to
/// key on, `weaver hook` must be a silent no-op: exit 0, print nothing, and — the
/// load-bearing part — write no event that would stamp the parent branch's
/// lifecycle mid-turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn hook_without_weaver_branch_is_a_silent_no_op() {
    let env = Env::start().await;
    let out = std::process::Command::new(weaver_bin())
        .args(["hook", "--event", "idle"])
        .env("WEAVER_API", format!("http://{}", env.addr))
        .env_remove("WEAVER_BRANCH")
        .env_remove("LOOM_TOKEN")
        .output()
        .expect("failed to spawn weaver");
    assert!(out.status.success(), "the hook must never fail the agent");
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "a branchless hook must be silent: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // And it recorded nothing against the seeded branch — the whole point.
    let log = env.run(&["log"]);
    assert!(
        !log.contains("idle") && !log.contains("hook"),
        "a branchless hook must not write an event: {log}"
    );
}

/// `weaver readme` prints the full weaver workflow guide so an agent can pull
/// the rules back on demand (e.g. after a compaction replayed only the catch-up).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn readme_prints_the_full_weaver_guide() {
    let env = Env::start().await;
    let out = env.run(&["readme"]);
    assert!(
        out.contains("weaver session"),
        "readme should print the WEAVER.md guide: {out}"
    );
    assert!(
        out.contains("weaver status"),
        "readme should describe the weaver CLI: {out}"
    );
}

/// On a genuine start/resume/clear (no `compact` source), the session-start hook
/// injects the full WEAVER.md primer as `additionalContext`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn session_start_hook_injects_the_full_primer() {
    let env = Env::start().await;
    let payload = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
    let out = env.run_with_stdin(&["hook", "--event", "session-start"], payload);
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn session_start_hook_after_compaction_replays_the_concise_summary() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifact", "write", "goal"], "ship the feature\n");
    env.run(&["issue", "add", "wire", "up", "routes"]);
    env.run(&["status", "ok", "routes", "wired"]);

    let payload = r#"{"hook_event_name":"SessionStart","source":"compact"}"#;
    let out = env.run_with_stdin(&["hook", "--event", "session-start"], payload);

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
    let log = env.run(&["log"]);
    assert!(
        log.contains("session-start"),
        "the hook should record a session-start event: {log}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_status_with_no_id_reports_current_branch() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifact", "write", "goal"], "do the thing\n");
    env.run(&["issue", "add", "step", "one"]);
    let out = env.run(&["status"]);
    assert!(out.contains("branch:      feature-test"), "status: {out}");
    assert!(out.contains("goal:        do the thing"), "status: {out}");
    assert!(out.contains("open issues: 1"), "status: {out}");
    // A fresh branch defaults to the calm `ok` attention level.
    assert!(out.contains("status:      ok"), "status: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_status_sets_level_and_message() {
    let env = Env::start().await;
    // Declare a level with a message, then read it back.
    let out = env.run(&["status", "attention", "Waiting", "for", "PR", "feedback"]);
    assert!(
        out.contains("attention — Waiting for PR feedback"),
        "set output: {out}"
    );

    let out = env.run(&["status"]);
    assert!(
        out.contains("status:      attention — Waiting for PR feedback"),
        "status read: {out}"
    );

    // A new message replaces the old one.
    env.run(&["status", "ok", "back", "to", "work"]);
    let out = env.run(&["status"]);
    assert!(
        out.contains("status:      ok — back to work"),
        "status read: {out}"
    );

    // A bare level change keeps the last message (the message is the persistent
    // current-state note; only the level is volatile).
    env.run(&["status", "blocked"]);
    let out = env.run(&["status"]);
    assert!(
        out.contains("status:      blocked — back to work"),
        "message should persist across a bare level change: {out}"
    );

    // The set also writes a `tag` event to the branch log (the attention tag).
    let log = env.run(&["log"]);
    assert!(log.contains("tag"), "log should record tag events: {log}");
    assert!(
        log.contains("attention"),
        "the tag event should carry the attention key: {log}"
    );
}

/// `weaver tag set triage` stamps the watch's mark on a *named* session —
/// a status axis distinct from the agent's own `attention` — and records a `tag`
/// event for the audit trail. The agent's attention tag is never touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn triage_tag_marks_a_session_without_touching_attention() {
    let env = Env::start().await;
    // The agent declares its own attention about itself.
    env.run(&["status", "blocked", "build", "broke"]);

    // No triage tag until a watch looks.
    let out = env.run(&["tag", "ls", "--session", "feature-test"]);
    assert!(
        !out.contains("triage"),
        "fresh session has no triage tag: {out}"
    );

    // A watch stamps a *different* opinion on the same session via the
    // triage tag.
    let out = env.run(&[
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
    ]);
    assert!(out.contains("triage = attention"), "triage tag set: {out}");

    // Read it back with its note and attribution.
    let out = env.run(&["tag", "ls", "--session", "feature-test"]);
    assert!(out.contains("triage = attention"), "read level: {out}");
    assert!(out.contains("looks stuck on tests"), "read note: {out}");
    assert!(out.contains("status-check"), "read attribution: {out}");

    // The agent's own attention is untouched — two actors, two axes. Its tag
    // sits alongside the triage tag.
    assert!(
        out.contains("attention = blocked"),
        "agent attention must survive a triage write: {out}"
    );
    let out = env.run(&["status"]);
    assert!(
        out.contains("status:      blocked — build broke"),
        "the resolved status reads the agent's attention tag: {out}"
    );

    // The mark is logged as a `tag` event.
    let log = env.run(&["log"]);
    assert!(log.contains("tag"), "log should record tag events: {log}");

    // Clearing the triage tag leaves the agent's attention untouched.
    env.run(&["tag", "rm", "triage", "--session", "feature-test"]);
    let out = env.run(&["tag", "ls", "--session", "feature-test"]);
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
/// with a non-zero exit, like `status`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn tag_set_rejects_invalid_loud_value() {
    let env = Env::start().await;
    let out = env.run_raw(&["tag", "set", "triage", "bogus", "--session", "feature-test"]);
    assert!(!out.status.success(), "an invalid loud value should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("attention, blocked"),
        "stderr should name the valid values: {stderr}"
    );
}

/// `weaver tag set/ls/rm` round-trips a free-form (quiet) tag on the current
/// branch with its note and author, and `tag rm` clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn tag_set_ls_rm_roundtrip() {
    let env = Env::start().await;

    // No tags to begin with.
    let out = env.run(&["tag", "ls"]);
    assert!(out.contains("(no tags)"), "fresh branch has no tags: {out}");

    // Set a free-form tag with a note and author.
    let out = env.run(&[
        "tag",
        "set",
        "priority",
        "high",
        "--note",
        "ship by friday",
        "--by",
        "russell",
    ]);
    assert!(out.contains("priority = high"), "tag set: {out}");

    // List it back with its note and attribution.
    let out = env.run(&["tag", "ls"]);
    assert!(out.contains("priority = high"), "tag ls value: {out}");
    assert!(out.contains("by russell"), "tag ls author: {out}");
    assert!(out.contains("ship by friday"), "tag ls note: {out}");

    // Setting the same key again overwrites it (single-valued).
    env.run(&["tag", "set", "priority", "low"]);
    let out = env.run(&["tag", "ls"]);
    assert!(out.contains("priority = low"), "tag overwrite: {out}");
    assert_eq!(out.matches("priority").count(), 1, "single-valued: {out}");

    // Remove it.
    env.run(&["tag", "rm", "priority"]);
    let out = env.run(&["tag", "ls"]);
    assert!(out.contains("(no tags)"), "tag rm cleared it: {out}");
}

/// `status ok` clears the agent's `attention` tag (returns to calm) while
/// leaving the branch `description` in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_status_ok_clears_attention_tag_but_keeps_description() {
    let env = Env::start().await;
    // Raise attention with a message.
    env.run(&["status", "attention", "ready", "for", "review"]);
    let out = env.run(&["tag", "ls"]);
    assert!(
        out.contains("attention = attention"),
        "status should write the attention tag: {out}"
    );

    // Return to calm — the attention tag is cleared, the description survives.
    env.run(&["status", "ok"]);
    let out = env.run(&["tag", "ls"]);
    assert!(
        !out.contains("attention ="),
        "status ok should clear the attention tag: {out}"
    );
    let out = env.run(&["status"]);
    assert!(
        out.contains("status:      ok — ready for review"),
        "ok must keep the last description beside the calm level: {out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_status_rejects_unknown_level() {
    let env = Env::start().await;
    let out = env.run_raw(&["status", "bogus"]);
    assert!(!out.status.success(), "unknown level should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown status 'bogus'"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn config_get_ls_reads_settings() {
    let env = Env::start().await;
    // A known setting has a default value before anything is set.
    let out = env.run(&["config", "ls"]);
    assert!(out.contains("(default)"), "ls should mark defaults: {out}");

    // Settings are written by operators (`loom config set` / the settings
    // pane); the in-session surface only reads them.
    weaver_core::config::apply(
        &env.db,
        &[("server.auto_adopt".to_string(), Some("true".to_string()))],
    )
    .await
    .unwrap();
    let out = env.run(&["config", "get", "server.auto_adopt"]);
    assert_eq!(out.trim(), "true");
    let out = env.run(&["config", "ls"]);
    assert!(
        out.contains("server.auto_adopt") && out.contains("true"),
        "ls shows the stored value: {out}"
    );
}
