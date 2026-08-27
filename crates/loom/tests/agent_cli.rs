//! Agent-facing CLI flows against a real (locally isolated) loom server — the
//! CLI's only mode: an HTTP-only client of loom (see
//! `weaver_api::endpoint`). Each test boots its own server on a random port
//! with an isolated `WEAVER_HOME`, seeds one branch and the session running on
//! it, and drives the `weaver` binary as a subprocess pointed at it via
//! `$WEAVER_API`/`$WEAVER_BRANCH`/`$LOOM_TOKEN` — the same env loom injects
//! into every session it launches.
//!
//! `Env::start` mutates process-global env (`WEAVER_HOME`), so every test is
//! `#[serial]` — they share one binary and would otherwise race on that env.

use std::io::Write;
use std::net::SocketAddr;
use std::process::{Command, Stdio};

use loom::session as session_mod;
use loom::AppState;
use loom::{auth, db, server};
use serial_test::serial;
use tokio::net::TcpListener;
use weaver_core::events::EventBus;

#[path = "support/schema.rs"]
mod support_schema;
use support_schema::seed_migrated_db;

/// The title and goal every fixture branch carries. The session channel is
/// named after the one and opens with the other.
const BRANCH_TITLE: &str = "CLI channel";
const BRANCH_GOAL: &str = "coordinate durably";

/// Path to the freshly-built `weaver` binary the test will drive.
fn loom_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_loom"))
}

/// Seed the session running on `branch_id`, as loom's launcher would.
///
/// The `NewSession` fields are ordinary launch defaults; what matters to this
/// suite is that a session row exists at all, because a session credential is
/// how the server resolves the caller's own repository, branch, and session for
/// every declaration-served command.
async fn seed_session(db: &weaver_core::db::Db, branch_id: &str) -> loom::session::Session {
    session_mod::insert_with_policy(
        db,
        &session_mod::NewSession {
            id: "sess-agent-cli".to_string(),
            branch_id: branch_id.to_string(),
            work_dir: "/repo".to_string(),
            term_session: "weaver-sess-agent-cli".to_string(),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: None,
            protocol: "acp".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        },
        &launch_policy(),
    )
    .await
    .unwrap()
}

/// An ordinary unrestricted launch policy — what loom assigns to a session
/// it starts. `restricted: false` mints a credential carrying the full session
/// capability set, as a real agent's does.
fn launch_policy() -> session_mod::SessionLaunchPolicy {
    session_mod::SessionLaunchPolicy {
        profile: loom::profile::DEFAULT_PROFILE.to_string(),
        launch_mode: "auto".to_string(),
        profile_revision: 1,
        profile_lifetime: 1,
        strict: false,
        env_clear: false,
        ambient_allowlist: "[]".to_string(),
        idle_archive_secs: None,
        turn_budget: 0,
        prelude: "weaver".to_string(),
        restricted: false,
        github_repositories: "[]".to_string(),
        allowed_tools: "[]".to_string(),
        mcp_access: r#"{"selection":{"mode":"none","groups":[]},"capability_sets":[]}"#.to_string(),
        launch_snapshot: String::new(),
        creator_kind: "system".to_string(),
        creator_subject: "test".to_string(),
        parent_session_id: None,
        automation_run_id: None,
    }
}

/// A running loom server, isolated in its own temp `WEAVER_HOME`/sqlite db,
/// with one branch row seeded — the target `$WEAVER_BRANCH` names, exactly as
/// loom would set it for a real session — plus the session running on it and
/// its scoped credential.
struct Env {
    addr: SocketAddr,
    branch_id: String,
    repo_root: String,
    branch_name: String,
    session_id: String,
    /// The session credential loom hands an agent as `$LOOM_TOKEN`.
    ///
    /// A declaration-served command resolves `repo_root`, `branch` and
    /// `session` from the caller's own credential, so without a session token
    /// every operand the dispatcher supplies would be empty.
    token: String,
    db: weaver_core::db::Db,
    _home: tempfile::TempDir,
}

impl Env {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        seed_migrated_db();
        // This suite's requests (the `weaver` CLI, over loopback) need a seeded
        // owner to resolve to.
        std::env::set_var("LOOM_OWNER_GITHUB", "rjpower");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pool = db::connect(&db::default_db_path()).await.unwrap();

        let repo_root = "/repo".to_string();
        let branch_name = "feature-test".to_string();
        let branch = weaver_core::branch::upsert(&pool, &repo_root, &branch_name, "main")
            .await
            .unwrap();
        // Both before the session: a session channel is named after the branch
        // title and opens with its goal, and neither reaches back afterwards.
        weaver_core::branch::set_title(
            &pool,
            &branch.id,
            BRANCH_TITLE,
            weaver_core::branch::TitleProvenance::User,
        )
        .await
        .unwrap();
        weaver_core::branch::set_goal(&pool, &branch.id, BRANCH_GOAL, "user")
            .await
            .unwrap();

        let trigger = loom::github_trigger::GithubTrigger::production(pool.clone());
        let state = AppState {
            ctx: loom::Ctx {
                db: pool.clone(),
                bus: EventBus::new(),
                addr: addr.to_string(),
            },
            ide: std::sync::Arc::new(loom::ide::IdeManager::new(loom::ide::ide_home())),
            trigger,
            acp: loom::acp::AcpRegistry::new(),
            launch_gate: loom::launch_gate::RepoLaunchGate::default(),
        };
        tokio::spawn(server::serve(state, listener));

        let session = seed_session(&pool, &branch.id).await;
        let token = auth::create_session_token(&pool, None, &session.id, &branch.id)
            .await
            .unwrap();

        let env = Env {
            addr,
            branch_id: branch.id,
            repo_root,
            branch_name,
            session_id: session.id,
            token,
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
        let mut cmd = Command::new(loom_bin());
        cmd.args(args)
            .env("WEAVER_API", format!("http://{}", self.addr))
            .env("WEAVER_BRANCH", &self.branch_id)
            .env("LOOM_TOKEN", &self.token);
        cmd
    }

    /// Run the Loom binary with the given args, returning captured stdout.
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

    /// Run the Loom binary with `stdin` piped in, returning captured stdout.
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

    /// Run the Loom binary and return the raw output (not asserting on
    /// success) — for tests exercising a failure path.
    fn run_raw(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().expect("failed to spawn weaver")
    }
}

/// The goal lives as the `goal` artifact; writing it keeps the branch's
/// denormalized goal (what `loom status` reads back) in sync.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn goal_artifact_write_syncs_the_branch_goal() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifacts", "write", "goal"], "ship the thing\n");
    let out = env.run(&["artifacts", "show", "goal"]);
    assert_eq!(out.trim(), "ship the thing");
    let out = env.run(&["status", "get"]);
    assert!(out.contains("goal:        ship the thing"), "status: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn where_reports_resolved_branch() {
    let env = Env::start().await;
    let out = env.run(&["self"]);
    assert!(
        out.contains("branch:    feature-test"),
        "where output: {out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_weaver_branch_gives_a_friendly_error() {
    let env = Env::start().await;
    let out = std::process::Command::new(loom_bin())
        .args(["self"])
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
    let out = std::process::Command::new(loom_bin())
        .args(["self"])
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
    env.run(&["issues", "add", "fix", "the", "thing"]);
    env.run(&["issues", "add", "another", "task"]);

    let out = env.run(&["issues", "ls"]);
    assert!(out.contains("fix the thing"), "ls output: {out}");
    assert!(out.contains("another task"), "ls output: {out}");
    assert_eq!(out.matches("[ ]").count(), 2, "two open issues");

    // `close`, `reopen` and `rm` answer with the items they changed, not a count.
    let closed = env.run(&["issues", "close", "1"]);
    assert!(closed.contains("closed #1"), "close: {closed}");
    let out = env.run(&["issues", "ls"]);
    assert!(
        !out.contains("fix the thing"),
        "closed issue should be hidden"
    );

    let out = env.run(&["issues", "ls", "--all"]);
    assert!(
        out.contains("[x]"),
        "closed marker should appear with --all"
    );

    let reopened = env.run(&["issues", "reopen", "1"]);
    assert!(reopened.contains("reopened #1"), "reopen: {reopened}");
    let out = env.run(&["issues", "ls"]);
    assert_eq!(out.matches("[ ]").count(), 2);

    let removed = env.run(&["issues", "rm", "1"]);
    assert!(removed.contains("deleted #1"), "rm: {removed}");
    let out = env.run(&["issues", "ls", "--all"]);
    assert!(!out.contains("fix the thing"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn channel_cli_reads_and_appends_typed_history() {
    let env = Env::start().await;
    // The session channel is the one `Env` seeded; a branch holds only one.
    let channel = env.session_id.clone();

    let listed = env.run(&["channels", "ls"]);
    assert!(listed.contains(&channel), "channel list: {listed}");
    assert!(listed.contains(BRANCH_TITLE), "channel list: {listed}");

    let opening = env.run(&["channels", "read", "--channel", &channel]);
    assert!(opening.contains("goal"), "channel history: {opening}");
    assert!(opening.contains(BRANCH_GOAL), "channel history: {opening}");

    let sent = env.run(&[
        "channels",
        "send",
        "--channel",
        &channel,
        "--kind",
        "result",
        "ready",
        "for",
        "review",
    ]);
    assert!(sent.contains("result"), "channel send: {sent}");
    assert!(sent.contains("ready for review"), "channel send: {sent}");

    let history = env.run(&["channels", "read", "--channel", &channel]);
    assert!(
        history.contains("ready for review"),
        "channel history: {history}"
    );

    // `get` reads its delivery bindings out of the channel it already fetched,
    // and `ack` reports the marker the server moved.
    let detail = env.run(&["channels", "get", &channel]);
    assert!(
        detail.contains(&format!("id:      {channel}")),
        "channel get: {detail}"
    );
    assert!(detail.contains("bindings:"), "channel get: {detail}");

    let acked = env.run(&["channels", "ack", "--channel", &channel]);
    assert!(
        acked.contains(&format!("{channel} read through")),
        "channel ack: {acked}"
    );

    let subscribed = env.run(&[
        "channels",
        "subscribe",
        "--channel",
        &channel,
        "--mode",
        "observe",
    ]);
    assert!(
        subscribed.contains(&channel) && subscribed.contains("observe through"),
        "channel subscribe: {subscribed}"
    );
}

/// `issue tag set` sets a free-form label, `issue show` prints it, and
/// `issue tag rm` clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_tag_set_show_clear() {
    let env = Env::start().await;
    env.run(&["issues", "add", "label", "me"]);

    env.run(&["issues", "tag", "set", "1", "priority", "high"]);
    let out = env.run(&["issues", "show", "1"]);
    assert!(out.contains("priority=high"), "show output: {out}");

    // A second set overwrites the value in place (single-valued per key).
    env.run(&["issues", "tag", "set", "1", "priority", "low"]);
    let out = env.run(&["issues", "show", "1"]);
    assert!(out.contains("priority=low"), "show output: {out}");
    assert!(!out.contains("priority=high"));

    env.run(&["issues", "tag", "rm", "1", "priority"]);
    let out = env.run(&["issues", "show", "1"]);
    assert!(!out.contains("priority="), "tag should be cleared: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_ls_separates_branch_work_from_repo_backlog() {
    let env = Env::start().await;
    // Default add → claimed by this branch. `--repo` → unclaimed backlog.
    env.run(&["issues", "add", "my", "task"]);
    env.run(&["issues", "add", "--repo", "backlog", "task"]);

    // Default ls shows both, under separate sections.
    let out = env.run(&["issues", "ls"]);
    assert!(out.contains("On this branch"), "ls: {out}");
    assert!(out.contains("my task"), "ls: {out}");
    assert!(out.contains("Repo backlog"), "ls: {out}");
    assert!(out.contains("backlog task"), "ls: {out}");

    // `--mine` drops the backlog section.
    let out = env.run(&["issues", "ls", "--mine"]);
    assert!(out.contains("my task"), "mine: {out}");
    assert!(
        !out.contains("backlog task"),
        "mine should hide backlog: {out}"
    );

    // The badge counts only this branch's claimed work, not the backlog.
    let out = env.run(&["status", "get"]);
    assert!(out.contains("open issues: 1"), "status: {out}");
}

/// `issue show` reports the live status of the branch working the issue,
/// letting a parent agent poll a delegated sub-tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn issue_show_includes_the_working_branch_status() {
    let env = Env::start().await;
    env.run(&["issues", "add", "the", "sub-task"]);
    // The current branch claims it; give the branch a live status.
    env.run(&[
        "status",
        "set",
        "--tag",
        "blocked",
        "--message",
        "build is broken",
    ]);
    let out = env.run(&["issues", "show", "1"]);
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
    env.run(&["issues", "add", "done", "already"]);
    env.run(&["issues", "close", "1"]);
    let out = env.run(&["issues", "wait", "1", "--timeout", "1"]);
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
    env.run(&["issues", "add", "still", "going"]);
    let out = env.run_raw(&["issues", "wait", "1", "--timeout", "1", "--interval", "1"]);
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

    let out = env.run(&["issues", "ls"]);
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
///
/// Also seeds the child's own session, parented to the launching branch: the
/// parent's poll of its sub-tree depends on that session-tree link, not the
/// issue's `source_branch`. Without it, the poll is correctly refused.
async fn seed_delegated_issue(env: &Env, child: &str, attention: &str, description: &str) {
    let child_id = weaver_core::branch::new_id();
    weaver_core::branch::insert(&env.db, &child_id, &env.repo_root, child, "main")
        .await
        .unwrap();
    session_mod::insert_with_policy(
        &env.db,
        &session_mod::NewSession {
            id: format!("sess-{child_id}"),
            branch_id: child_id.clone(),
            work_dir: env.repo_root.clone(),
            term_session: format!("weaver-{child_id}"),
            agent_kind: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "running".to_string(),
            github_repo: None,
            parent_branch_id: Some(env.branch_id.clone()),
            managed_by: None,
            created_by: None,
            protocol: "acp".to_string(),
            origin: "agent".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        },
        &session_mod::SessionLaunchPolicy {
            parent_session_id: Some(env.session_id.clone()),
            ..launch_policy()
        },
    )
    .await
    .unwrap();
    // The attention level lives on the `attention` tag; `ok` is absence.
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

/// `summary` is the agent catch-up: it prints the goal, the current status,
/// the explicit backlog, and a generated next-step hint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn summary_orients_an_agent_on_the_branch() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifacts", "write", "goal"], "ship the feature\n");
    env.run(&["issues", "add", "wire", "up", "routes"]);
    env.run(&["issues", "add", "add", "tests"]);
    env.run(&["status", "set", "--tag", "ok", "--message", "routes wired"]);

    let out = env.run(&["summary"]);
    assert!(out.contains("ship the feature"), "summary: {out}");
    // The current status (level + message) is the where-you-left-off signal.
    assert!(out.contains("ok — routes wired"), "summary: {out}");
    // Backlog lists intentional work items themselves, not just a count.
    assert!(out.contains("Backlog (2):"), "summary: {out}");
    assert!(out.contains("#1    wire up routes"), "summary: {out}");
    assert!(out.contains("#2    add tests"), "summary: {out}");
    // The next-action hint points at the first open task.
    assert!(out.contains("pick up #1"), "summary: {out}");
    // Every section advertises the command that drills into it.
    for hint in [
        "(loom artifacts get goal)",
        "(loom status get)",
        "(loom issues list)",
        "loom artifacts",
        "loom sessions events",
    ] {
        assert!(out.contains(hint), "summary should surface `{hint}`: {out}");
    }
}

/// The backlog list is capped (across own issues *and* legacy delegated items)
/// so a branch with lots of work can't blow up the summary; the overflow
/// collapses into a single "(+N more)" line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn summary_caps_a_long_outstanding_list() {
    let env = Env::start().await;
    for n in 0..13 {
        let title = format!("task{n}");
        env.run(&["issues", "add", title.as_str()]);
    }
    let out = env.run(&["summary"]);
    assert!(out.contains("Backlog (13):"), "summary: {out}");
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
    env.run_with_stdin(&["artifacts", "write", "goal"], "tidy up\n");
    let out = env.run(&["summary"]);
    assert!(out.contains("Backlog: none"), "summary: {out}");
    assert!(out.contains("no explicit backlog items"), "summary: {out}");
    assert!(out.contains("open a PR"), "summary: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_write_show_ls_and_revisions() {
    let env = Env::start().await;
    // Write from stdin (no file arg); the CLI prints a dashboard URL and the
    // new revision.
    let out = env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan\n\nDesign here.\n");
    assert!(
        out.contains("/artifacts/plan"),
        "write should print the URL: {out}"
    );
    assert!(out.contains("rev 1"), "first write is rev 1: {out}");

    // show prints the content verbatim.
    let shown = env.run(&["artifacts", "show", "plan"]);
    assert!(shown.contains("Design here."), "show: {shown}");

    // A second write appends rev 2, and --rev 1 still fetches the original.
    let out2 = env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan v2\n");
    assert!(out2.contains("rev 2"), "second write is rev 2: {out2}");
    let v1 = env.run(&["artifacts", "show", "plan", "--rev", "1"]);
    assert!(v1.contains("Design here."), "rev 1 is the original: {v1}");

    // --meta prints the envelope, not the content.
    let meta = env.run(&["artifacts", "show", "plan", "--meta"]);
    assert!(meta.contains("name:    plan"), "meta: {meta}");
    assert!(meta.contains("rev:     2"), "meta latest rev: {meta}");
    assert!(
        meta.contains("branch"),
        "meta scope is branch-scoped: {meta}"
    );

    // ls lists the branch-scoped artifact.
    let ls = env.run(&["artifacts", "ls"]);
    assert!(ls.contains("plan"), "ls: {ls}");
    assert!(ls.contains("rev 2"), "ls shows latest rev: {ls}");

    // A --repo write is repo-shared; --repo ls shows it.
    env.run_with_stdin(&["artifacts", "write", "shared", "--repo"], "shared body\n");
    let repo_ls = env.run(&["artifacts", "ls", "--repo"]);
    assert!(
        repo_ls.contains("repo:shared"),
        "repo ls shows shared scope: {repo_ls}"
    );

    // rm reports the scope and revision it removed, then the artifact is gone.
    let rm = env.run(&["artifacts", "rm", "plan"]);
    assert!(rm.contains("was rev 2"), "rm: {rm}");
    let ls = env.run(&["artifacts", "ls"]);
    assert!(!ls.contains("plan"), "rm should remove it: {ls}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_history_lists_every_revision() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan\n");
    env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan v2\n");

    let history = env.run(&["artifacts", "history", "plan"]);
    assert!(history.contains("rev 1"), "history: {history}");
    assert!(history.contains("rev 2"), "history: {history}");
    assert!(
        history.contains("agent"),
        "history names the author: {history}"
    );
}

/// `resolve` answers with the thread as it now stands — `[resolved]` — instead
/// of echoing back the id the caller passed in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_resolve_reports_the_resolved_thread() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan\n\nDesign here.\n");
    let opened = env.run(&[
        "artifacts",
        "comment",
        "plan",
        "--quote",
        "Design here.",
        "needs detail",
    ]);
    let tid: i64 = opened
        .split_whitespace()
        .nth(2)
        .expect("opened thread <id>")
        .parse()
        .expect("thread id is numeric");

    let resolved = env.run(&["artifacts", "resolve", "plan", &tid.to_string()]);
    assert!(
        resolved.contains(&format!("#{tid} [resolved]")),
        "resolve reports the thread's new state: {resolved}"
    );
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
    let derived = env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan\n");
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
    weaver_core::config::apply(
        &env.db,
        &[(
            "auth.base_url".to_string(),
            Some("https://loom.example.com".to_string()),
        )],
    )
    .await
    .unwrap();
    let public = env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan v2\n");
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
    env.run_with_stdin(&["artifacts", "write", "plan"], "# Plan\n\nDesign here.\n");

    // No thread yet.
    let threads = env.run(&["artifacts", "threads", "plan"]);
    assert!(
        threads.contains("no open threads"),
        "no threads yet: {threads}"
    );

    // Opening a thread requires --quote.
    let missing_quote = env.run_raw(&["artifacts", "comment", "plan", "looks off"]);
    assert!(
        !missing_quote.status.success(),
        "comment without --quote or --thread should fail"
    );

    // Open a new thread anchored to a quote, seeded with its first comment.
    let opened = env.run(&[
        "artifacts",
        "comment",
        "plan",
        "--quote",
        "Design here.",
        "this needs more detail",
    ]);
    assert!(opened.contains("opened thread"), "opened: {opened}");

    let threads = env.run(&["artifacts", "threads", "plan"]);
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
        "artifacts",
        "comment",
        "plan",
        "--thread",
        &tid.to_string(),
        "fixed, take a look",
    ]);
    assert!(replied.contains("added comment"), "replied: {replied}");

    let threads = env.run(&["artifacts", "threads", "plan"]);
    assert!(threads.contains("fixed, take a look"), "{threads}");

    env.run(&["artifacts", "resolve", "plan", &tid.to_string()]);
    let threads = env.run(&["artifacts", "threads", "plan"]);
    assert!(
        threads.contains("no open threads"),
        "resolved thread should no longer be open: {threads}"
    );
    let all = env.run(&["artifacts", "threads", "plan", "--all"]);
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
    let log = env.run(&["sessions", "events"]);
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
/// key on, `loom hook` must be a silent no-op: exit 0, print nothing, and — the
/// load-bearing part — write no event that would stamp the parent branch's
/// lifecycle mid-turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn hook_without_weaver_branch_is_a_silent_no_op() {
    let env = Env::start().await;
    let out = std::process::Command::new(loom_bin())
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
    // It also recorded nothing against the seeded branch.
    let log = env.run(&["sessions", "events"]);
    assert!(
        !log.contains("idle") && !log.contains("hook"),
        "a branchless hook must not write an event: {log}"
    );
}

/// `loom help` renders the code-registered operation catalogue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn help_prints_the_registered_loom_guide() {
    let env = Env::start().await;
    let out = env.run(&["help"]);
    assert!(
        out.contains("Loom's registered operation groups"),
        "help should print the registered Loom catalogue: {out}"
    );
    assert!(
        out.contains("permissions"),
        "help should list registered Loom groups: {out}"
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
        out.contains("detached Loom session"),
        "startup should replay the registered primer: {out}"
    );
    assert!(
        !out.contains("Context was just compacted"),
        "startup must not use the compaction replay: {out}"
    );
}

/// After a context compaction (`source: "compact"`), the hook replays a concise
/// re-orientation — the summary catch-up plus the load-bearing rules and a
/// pointer to registered Loom help — instead of the full primer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn session_start_hook_after_compaction_replays_the_concise_summary() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifacts", "write", "goal"], "ship the feature\n");
    env.run(&["issues", "add", "wire", "up", "routes"]);
    env.run(&["status", "set", "--tag", "ok", "--message", "routes wired"]);

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
        out.contains("loom help"),
        "replay should point at registered help: {out}"
    );
    // It must stay concise — not re-feed the whole WEAVER.md.
    assert!(
        !out.contains("detached Loom session"),
        "compact replay must not dump the full primer: {out}"
    );

    // The hook still records the lifecycle event (with its source) for the monitor.
    let log = env.run(&["sessions", "events"]);
    assert!(
        log.contains("session-start"),
        "the hook should record a session-start event: {log}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn get_status_reports_current_branch() {
    let env = Env::start().await;
    env.run_with_stdin(&["artifacts", "write", "goal"], "do the thing\n");
    env.run(&["issues", "add", "step", "one"]);
    let out = env.run(&["status", "get"]);
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
    let out = env.run(&[
        "status",
        "set",
        "--tag",
        "attention",
        "--message",
        "Waiting for PR feedback",
    ]);
    assert!(
        out.contains("attention — Waiting for PR feedback"),
        "set output: {out}"
    );

    let out = env.run(&["status", "get"]);
    assert!(
        out.contains("status:      attention — Waiting for PR feedback"),
        "status read: {out}"
    );

    // A new message replaces the old one.
    env.run(&["status", "set", "--tag", "ok", "--message", "back to work"]);
    let out = env.run(&["status", "get"]);
    assert!(
        out.contains("status:      ok — back to work"),
        "status read: {out}"
    );

    // A bare level change keeps the last message (the message is the persistent
    // current-state note; only the level is volatile).
    env.run(&["status", "set", "--tag", "blocked"]);
    let out = env.run(&["status", "get"]);
    assert!(
        out.contains("status:      blocked — back to work"),
        "message should persist across a bare level change: {out}"
    );

    // The set also writes a `tag` event to the branch log (the attention tag).
    let log = env.run(&["sessions", "events"]);
    assert!(log.contains("tag"), "log should record tag events: {log}");
    assert!(
        log.contains("attention"),
        "the tag event should carry the attention key: {log}"
    );
}

/// `loom sessions tags set triage` stamps the watch's mark on a *named* session —
/// a status axis distinct from the agent's own `attention` — and records a `tag`
/// event for the audit trail. The agent's attention tag is never touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn triage_tag_marks_a_session_without_touching_attention() {
    let env = Env::start().await;
    // The agent declares its own attention about itself.
    env.run(&[
        "status",
        "set",
        "--tag",
        "blocked",
        "--message",
        "build broke",
    ]);

    // No triage tag until a watch looks.
    let out = env.run(&["sessions", "tags", "ls", "--session", "feature-test"]);
    assert!(
        !out.contains("triage"),
        "fresh session has no triage tag: {out}"
    );

    // A watch stamps a *different* opinion on the same session via the
    // triage tag.
    let out = env.run(&[
        "sessions",
        "tags",
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
    let out = env.run(&["sessions", "tags", "ls", "--session", "feature-test"]);
    assert!(out.contains("triage = attention"), "read level: {out}");
    assert!(out.contains("looks stuck on tests"), "read note: {out}");
    assert!(out.contains("status-check"), "read attribution: {out}");

    // The agent's own attention is untouched — two actors, two axes. Its tag
    // sits alongside the triage tag.
    assert!(
        out.contains("attention = blocked"),
        "agent attention must survive a triage write: {out}"
    );
    let out = env.run(&["status", "get"]);
    assert!(
        out.contains("status:      blocked — build broke"),
        "the resolved status reads the agent's attention tag: {out}"
    );

    // The mark is logged as a `tag` event.
    let log = env.run(&["sessions", "events"]);
    assert!(log.contains("tag"), "log should record tag events: {log}");

    // Clearing the triage tag leaves the agent's attention untouched.
    env.run(&[
        "sessions",
        "tags",
        "rm",
        "triage",
        "--session",
        "feature-test",
    ]);
    let out = env.run(&["sessions", "tags", "ls", "--session", "feature-test"]);
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
    let out = env.run_raw(&[
        "sessions",
        "tags",
        "set",
        "triage",
        "bogus",
        "--session",
        "feature-test",
    ]);
    assert!(!out.status.success(), "an invalid loud value should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("attention, blocked"),
        "stderr should name the valid values: {stderr}"
    );
}

/// `loom sessions tags set/list/delete` round-trips a free-form (quiet) tag on the current
/// branch with its note and author, and `tag rm` clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn tag_set_ls_rm_roundtrip() {
    let env = Env::start().await;

    // No tags to begin with.
    let out = env.run(&["sessions", "tags", "ls"]);
    assert!(out.contains("(no tags)"), "fresh branch has no tags: {out}");

    // Set a free-form tag with a note and author.
    let out = env.run(&[
        "sessions",
        "tags",
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
    let out = env.run(&["sessions", "tags", "ls"]);
    assert!(out.contains("priority = high"), "tag ls value: {out}");
    assert!(out.contains("by russell"), "tag ls author: {out}");
    assert!(out.contains("ship by friday"), "tag ls note: {out}");

    // Setting the same key again overwrites it (single-valued).
    env.run(&["sessions", "tags", "set", "priority", "low"]);
    let out = env.run(&["sessions", "tags", "ls"]);
    assert!(out.contains("priority = low"), "tag overwrite: {out}");
    assert_eq!(out.matches("priority").count(), 1, "single-valued: {out}");

    // Remove it.
    env.run(&["sessions", "tags", "rm", "priority"]);
    let out = env.run(&["sessions", "tags", "ls"]);
    assert!(out.contains("(no tags)"), "tag rm cleared it: {out}");
}

/// `status set --tag ok` clears the agent's `attention` tag (returns to calm) while
/// leaving the branch `description` in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_status_ok_clears_attention_tag_but_keeps_description() {
    let env = Env::start().await;
    // Raise attention with a message.
    env.run(&[
        "status",
        "set",
        "--tag",
        "attention",
        "--message",
        "ready for review",
    ]);
    let out = env.run(&["sessions", "tags", "ls"]);
    assert!(
        out.contains("attention = attention"),
        "status should write the attention tag: {out}"
    );

    // Return to calm — the attention tag is cleared, the description survives.
    env.run(&["status", "set", "--tag", "ok"]);
    let out = env.run(&["sessions", "tags", "ls"]);
    assert!(
        !out.contains("attention ="),
        "calm status should clear the attention tag: {out}"
    );
    let out = env.run(&["status", "get"]);
    assert!(
        out.contains("status:      ok — ready for review"),
        "ok must keep the last description beside the calm level: {out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn set_status_rejects_unknown_level() {
    let env = Env::start().await;
    let out = env.run_raw(&["status", "set", "--tag", "bogus"]);
    assert!(!out.status.success(), "unknown level should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown status 'bogus'"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_requires_an_explicit_get_or_set_verb() {
    let env = Env::start().await;
    let missing = env.run_raw(&["status"]);
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("a subcommand is required"),
        "stderr: {stderr}"
    );

    let legacy = env.run_raw(&["status", "attention"]);
    assert!(!legacy.status.success(), "legacy status syntax should fail");
    let stderr = String::from_utf8_lossy(&legacy.stderr);
    assert!(
        stderr.contains("unrecognized subcommand 'attention'"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn config_get_ls_reads_settings() {
    let env = Env::start().await;
    // A known setting has a default value before anything is set.
    let out = env.run(&["settings", "ls"]);
    assert!(out.contains("(default)"), "ls should mark defaults: {out}");

    // Settings are written by operators (`loom config set` / the settings
    // pane); the in-session `settings` command only reads them.
    weaver_core::config::apply(
        &env.db,
        &[("server.auto_adopt".to_string(), Some("true".to_string()))],
    )
    .await
    .unwrap();
    let out = env.run(&["settings", "get", "server.auto_adopt"]);
    assert_eq!(out.trim(), "true");
    let out = env.run(&["settings", "ls"]);
    assert!(
        out.contains("server.auto_adopt") && out.contains("true"),
        "ls shows the stored value: {out}"
    );
}
