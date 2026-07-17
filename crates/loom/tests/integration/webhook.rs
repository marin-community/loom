//! The inbound GitHub trigger end-to-end over HTTP: a signed `issue_comment`
//! delivery to `POST /api/github/webhook` turns `@loom work on this` into a
//! session and replies with its URL. The security boundary is exercised here —
//! a bad/missing signature is a hard 401, a replay is a no-op, a non-trigger or
//! unauthorized comment launches nothing.
//!
//! No network and no real `gh`: the clone source is a *local bare repo* and the
//! GitHub gateway (permission check + reply) is a recording fake installed into
//! the server via [`TestServer::start_with_github`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use serde_json::json;
use serial_test::serial;
use sha2::Sha256;

use crate::fixtures::{sh, TestServer};

const SECRET: &str = "test-webhook-secret";

/// Forge the `X-Hub-Signature-256` value a genuine GitHub delivery carries for
/// `(secret, body)`.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// A recording GitHub gateway standing in for `gh`: it captures every reply
/// posted and answers `pr_head` with a value a PR-flow test can pin.
#[derive(Default)]
struct FakeGithub {
    comments: Mutex<Vec<(String, i64, String)>>,
    /// What `pr_head` returns; a PR-flow test sets this to a branch it created in
    /// the fixture remote. `None` → a plain same-repo head named `feature`.
    pr_head: Mutex<Option<loom::github_trigger::PrHead>>,
}

#[async_trait::async_trait]
impl loom::github_trigger::GithubApi for FakeGithub {
    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> anyhow::Result<()> {
        self.comments
            .lock()
            .unwrap()
            .push((repo.to_string(), issue, body.to_string()));
        Ok(())
    }

    async fn pr_head(
        &self,
        _repo: &str,
        _number: i64,
    ) -> anyhow::Result<loom::github_trigger::PrHead> {
        Ok(self
            .pr_head
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(loom::github_trigger::PrHead {
                head_ref: "feature".to_string(),
                cross_repo: false,
            }))
    }
}

/// Boot a test server with the webhook secret set and a fake GitHub gateway
/// installed, returning both so a test can drive permission/replies.
async fn boot() -> (TestServer, Arc<FakeGithub>) {
    std::env::set_var("LOOM_GITHUB_WEBHOOK_SECRET", SECRET);
    let fake = Arc::new(FakeGithub::default());
    let ts = TestServer::start_with_github(fake.clone()).await;
    (ts, fake)
}

/// Lay out a bare repo whose path tail is `acme/widgets` (so the registered slug
/// is `acme/widgets`) and return its `file://` clone URL. Mirrors the repo-store
/// suite; kept local so the two don't share a fixture.
fn make_bare_remote(root: &Path) -> String {
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    sh(&work, "git", &["init", "-q", "-b", "main"]);
    sh(&work, "git", &["config", "user.email", "t@t.test"]);
    sh(&work, "git", &["config", "user.name", "Test"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    sh(&work, "git", &["add", "."]);
    sh(&work, "git", &["commit", "-q", "-m", "init"]);
    // A second branch so a PR-head-attach test has an `origin/feature` to fetch.
    sh(&work, "git", &["branch", "feature"]);

    let bare = root.join("acme").join("widgets");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    sh(
        &work,
        "git",
        &[
            "clone",
            "--bare",
            "-q",
            &work.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    format!("file://{}", bare.display())
}

/// Register `acme/widgets` (pointing at a local bare remote) into the managed
/// allowlist, and make new sessions launch the fast `shell` agent. Returns the
/// remotes tempdir, which must stay alive until the clone has happened.
async fn prepare_repo(ts: &TestServer) -> tempfile::TempDir {
    let remotes = tempfile::tempdir().unwrap();
    let url = make_bare_remote(remotes.path());
    ts.client
        .post("/api/repos", json!({ "repo": url }))
        .await
        .unwrap();
    // The webhook builds a CreateReq with no agent, so it uses `agent.default`;
    // pin it to `shell` so the test doesn't try to launch a real claude.
    ts.client
        .patch("/api/settings", json!({ "agent.default": "shell" }))
        .await
        .unwrap();
    remotes
}

/// The raw JSON body of an `issue_comment.created` carrying `comment` from
/// `login` on issue `number` of `acme/widgets`.
fn trigger_body(login: &str, number: i64, comment: &str) -> Vec<u8> {
    json!({
        "action": "created",
        "issue": {"number": number, "title": "Make it faster", "body": "perf please"},
        "comment": {"body": comment, "user": {"login": login}},
        "repository": {"full_name": "acme/widgets"}
    })
    .to_string()
    .into_bytes()
}

/// Like [`trigger_body`], but the issue carries a `pull_request` link, so the
/// handler treats it as a PR comment (attaching to the PR's head branch).
fn trigger_body_pr(login: &str, number: i64, comment: &str) -> Vec<u8> {
    json!({
        "action": "created",
        "issue": {
            "number": number,
            "title": "Fix the tests",
            "body": "they are red",
            "pull_request": {"url": "https://api.github.com/repos/acme/widgets/pulls/7"}
        },
        "comment": {"body": comment, "user": {"login": login}},
        "repository": {"full_name": "acme/widgets"}
    })
    .to_string()
    .into_bytes()
}

/// POST a delivery to the webhook. `sig` is sent as `X-Hub-Signature-256` when
/// `Some`; the body bytes are sent verbatim (so a caller-computed signature over
/// the same bytes stays valid).
async fn post(
    ts: &TestServer,
    delivery: &str,
    sig: Option<String>,
    body: &[u8],
) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("http://{}/api/github/webhook", ts.addr))
        .header("X-GitHub-Event", "issue_comment")
        .header("X-GitHub-Delivery", delivery)
        .header("Content-Type", "application/json")
        .body(body.to_vec());
    if let Some(sig) = sig {
        req = req.header("X-Hub-Signature-256", sig);
    }
    req.send().await.unwrap()
}

/// How many sessions the fleet listing shows.
async fn session_count(ts: &TestServer) -> usize {
    ts.client
        .get("/api/sessions")
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .len()
}

/// The trigger's real work — clone, session create, reply — now runs in a detached
/// task *after* the webhook's `200`, so tests wait for the effect rather than
/// reading it synchronously. Poll until the fleet shows at least `n` sessions.
async fn wait_for_sessions(ts: &TestServer, n: usize) {
    for _ in 0..200 {
        if session_count(ts).await >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("expected >= {n} sessions, saw {}", session_count(ts).await);
}

/// Poll until the fake gateway has recorded at least `n` replies.
async fn wait_for_comments(fake: &FakeGithub, n: usize) {
    for _ in 0..200 {
        if fake.comments.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let have = fake.comments.lock().unwrap().len();
    panic!("expected >= {n} replies, saw {have}");
}

/// A missing signature, and one computed with the wrong secret, are both
/// rejected with 401 — and nothing is launched.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_and_missing_signature_are_rejected() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;
    let body = trigger_body("alice", 1, "@loom work on this");

    // No signature header at all.
    let resp = post(&ts, "d-nosig", None, &body).await;
    assert_eq!(resp.status(), 401, "missing signature must be unauthorized");

    // A signature over the right body but with the wrong secret.
    let wrong = sign("not-the-secret", &body);
    let resp = post(&ts, "d-wrongsig", Some(wrong), &body).await;
    assert_eq!(
        resp.status(),
        401,
        "wrong-secret signature must be unauthorized"
    );

    assert_eq!(
        session_count(&ts).await,
        0,
        "no session from a rejected delivery"
    );
    assert!(
        fake.comments.lock().unwrap().is_empty(),
        "no reply from a rejected delivery"
    );
}

/// A correctly-signed comment that is not the trigger phrase is acknowledged
/// (200) but launches nothing.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_trigger_comment_is_ignored() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;

    let body = trigger_body("rjpower", 1, "just a normal comment, nothing to see");
    let resp = post(&ts, "d-chatter", Some(sign(SECRET, &body)), &body).await;
    assert_eq!(resp.status(), 200, "a non-trigger comment is acknowledged");
    assert_eq!(
        session_count(&ts).await,
        0,
        "no session from a non-trigger comment"
    );
    assert!(fake.comments.lock().unwrap().is_empty());
}

/// The trigger is a **mention**, not a prefix: tagging the phrase mid-sentence
/// launches, while a mention that only appears inside a quote does not. (The
/// unit tests in `github_trigger` pin the matcher itself; this is the same
/// distinction through the real handler.)
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mid_comment_mention_triggers_and_a_quoted_one_does_not() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;

    // A quote-reply pastes the comment being answered; that is not a new trigger.
    let quoted = trigger_body("rjpower", 8, "> @loom work on this\n\nagreed — nice one");
    let resp = post(&ts, "d-quoted", Some(sign(SECRET, &quoted)), &quoted).await;
    assert_eq!(resp.status(), 200, "a quoted mention is acknowledged");
    assert_eq!(
        session_count(&ts).await,
        0,
        "no session from a mention that only appears in a quote"
    );
    assert!(fake.comments.lock().unwrap().is_empty());

    // ...but tagging it part-way through a comment does launch.
    let body = trigger_body("rjpower", 8, "nice catch — @loom take it from here please");
    let resp = post(&ts, "d-midtext", Some(sign(SECRET, &body)), &body).await;
    assert_eq!(resp.status(), 200);
    wait_for_sessions(&ts, 1).await;
    wait_for_comments(&fake, 1).await;
}

/// A commenter who is not an approved loom user launches nothing — but instead of
/// a silent drop, loom replies once with a friendly "request access" note so they
/// know how to proceed. (Repo write access is not itself a grant — only the
/// approved-user allowlist is; the per-repo rate limit bounds the reply.)
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthorized_commenter_gets_access_reply() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;

    let body = trigger_body("stranger", 5, "@loom work on this");
    let resp = post(&ts, "d-stranger", Some(sign(SECRET, &body)), &body).await;
    assert_eq!(
        resp.status(),
        200,
        "an unauthorized trigger is acknowledged, not errored"
    );

    // The reply is posted from a detached task; wait for it.
    wait_for_comments(&fake, 1).await;
    let comments = fake.comments.lock().unwrap().clone();
    assert_eq!(comments.len(), 1, "exactly one access-info reply");
    let (repo, issue, reply) = &comments[0];
    assert_eq!(repo, "acme/widgets");
    assert_eq!(*issue, 5);
    assert!(
        reply.contains("access"),
        "the reply tells them they need access: {reply}"
    );

    assert_eq!(
        session_count(&ts).await,
        0,
        "an unauthorized commenter launches nothing"
    );
}

/// The happy path: an approved loom user triggers a session, and loom replies on
/// the issue with the live session URL.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_creates_session_and_replies() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;
    assert_eq!(session_count(&ts).await, 0);

    // `rjpower` is the seeded approved user (see the fixtures' LOOM_OWNER_GITHUB).
    let body = trigger_body("rjpower", 42, "@loom work on this please");
    let resp = post(&ts, "d-happy", Some(sign(SECRET, &body)), &body).await;
    assert_eq!(resp.status(), 200);

    // The launch runs in a detached task after the 200; wait for it to land.
    wait_for_sessions(&ts, 1).await;

    // A session now exists, seeded from the issue title.
    let sessions = ts.client.get("/api/sessions").await.unwrap();
    let sessions = sessions.as_array().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "the trigger launched exactly one session"
    );
    let session = &sessions[0];
    let id = session["id"].as_str().unwrap().to_string();
    assert_eq!(
        session["branch"]["title"].as_str(),
        Some("Make it faster"),
        "the session title is seeded from the issue"
    );
    assert_eq!(
        session["created_by"].as_str(),
        Some("rjpower"),
        "the session is attributed to the commenting user, so its GH_TOKEN is theirs"
    );

    // loom replied on the triggering issue with the session URL.
    wait_for_comments(&fake, 1).await;
    let comments = fake.comments.lock().unwrap().clone();
    assert_eq!(comments.len(), 1, "exactly one reply");
    let (repo, issue, reply) = &comments[0];
    assert_eq!(repo, "acme/widgets");
    assert_eq!(*issue, 42);
    assert!(
        reply.starts_with("On it — http://"),
        "reply leads with the cue: {reply}"
    );
    assert!(
        reply.contains(&format!("/s/{id}")),
        "reply links the session: {reply}"
    );

    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}

/// A replayed (or GitHub-retried) delivery with a GUID we've already processed
/// is a no-op: no second session, no second reply.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_delivery_is_a_noop() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;

    let body = trigger_body("rjpower", 9, "@loom work on this");
    let sig = sign(SECRET, &body);

    let resp = post(&ts, "d-replay", Some(sig.clone()), &body).await;
    assert_eq!(resp.status(), 200);
    wait_for_sessions(&ts, 1).await;
    wait_for_comments(&fake, 1).await;

    // The exact same delivery again — same GUID, body, signature. The GUID dedupe
    // rejects it synchronously (before any spawn), so nothing new can appear.
    let resp = post(&ts, "d-replay", Some(sig), &body).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        session_count(&ts).await,
        1,
        "the replay launches nothing new"
    );
    assert_eq!(
        fake.comments.lock().unwrap().len(),
        1,
        "the replay posts no second reply"
    );

    let id = ts.client.get("/api/sessions").await.unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}

/// A second @loom comment on a thread that already has a live session forwards the
/// comment into that session (an ack, no duplicate) instead of spawning a new one.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeat_trigger_forwards_to_active_session() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;

    let body = trigger_body("rjpower", 42, "@loom start");
    assert_eq!(
        post(&ts, "d-first", Some(sign(SECRET, &body)), &body)
            .await
            .status(),
        200
    );
    // Wait for the first session to exist *and* its reply to land, so the second
    // delivery deterministically finds an active session to forward into (and its
    // ack is the second comment, not a race with the first).
    wait_for_sessions(&ts, 1).await;
    wait_for_comments(&fake, 1).await;

    // A second @loom on the same issue (new delivery) is forwarded, not forked.
    let body2 = trigger_body("rjpower", 42, "@loom also handle the edge case");
    assert_eq!(
        post(&ts, "d-second", Some(sign(SECRET, &body2)), &body2)
            .await
            .status(),
        200
    );
    wait_for_comments(&fake, 2).await;
    assert_eq!(
        session_count(&ts).await,
        1,
        "the repeat trigger spawns no duplicate session"
    );

    let comments = fake.comments.lock().unwrap().clone();
    assert_eq!(comments.len(), 2, "one ack per trigger");
    assert!(
        comments[1].2.contains("Passed your note"),
        "the repeat trigger is acked as a forward: {}",
        comments[1].2
    );

    let id = ts.client.get("/api/sessions").await.unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}

/// A follow-up @loom comment on a thread whose session is orphaned — its terminal
/// died but the DB row still counts as active — must not be dropped. loom fails to
/// forward into the dead terminal, retires the orphaned session, and launches a
/// fresh one on the same branch (the trigger goal carries the comment), replying
/// with the launch cue rather than a forward ack.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphaned_session_terminal_relaunches_instead_of_dropping() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;

    // First trigger → one session with a live terminal.
    let body = trigger_body("rjpower", 42, "@loom start");
    assert_eq!(
        post(&ts, "d-orphan-1", Some(sign(SECRET, &body)), &body)
            .await
            .status(),
        200
    );
    wait_for_sessions(&ts, 1).await;
    wait_for_comments(&fake, 1).await;

    let first_id = ts.client.get("/api/sessions").await.unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Orphan it: kill the terminal but leave the DB row active, then wait until the
    // terminal is genuinely gone so the next forward attempt deterministically fails.
    let term = format!("weaver-{first_id}");
    loom::backend::kill_session(&term).await.unwrap();
    for _ in 0..200 {
        if !loom::backend::has_session(&term).await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !loom::backend::has_session(&term).await,
        "the first session's terminal is dead before the second trigger"
    );

    // A second @loom comment on the same issue: forwarding into the dead terminal
    // fails, so loom retires the orphaned session and launches a fresh one.
    let body2 = trigger_body("rjpower", 42, "@loom pick this back up");
    assert_eq!(
        post(&ts, "d-orphan-2", Some(sign(SECRET, &body2)), &body2)
            .await
            .status(),
        200
    );

    // A fresh session appears (two rows now: the retired one + the new one) and a
    // second reply lands.
    wait_for_sessions(&ts, 2).await;
    wait_for_comments(&fake, 2).await;

    let sessions = ts.client.get("/api/sessions").await.unwrap();
    let sessions = sessions.as_array().unwrap().clone();
    let retired = sessions
        .iter()
        .find(|s| s["id"].as_str() == Some(first_id.as_str()))
        .expect("the orphaned session is still recorded");
    assert_eq!(
        retired["status"].as_str(),
        Some("error"),
        "the unreachable session was retired to a terminal status so the branch is free"
    );
    let fresh_id = sessions
        .iter()
        .find(|s| s["id"].as_str() != Some(first_id.as_str()))
        .expect("a fresh session was launched")["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The reply is the launch cue, not a forward ack — proving a relaunch happened
    // rather than the comment being silently swallowed by the dead terminal.
    let comments = fake.comments.lock().unwrap().clone();
    assert_eq!(comments.len(), 2, "one reply per trigger");
    assert!(
        comments[1].2.starts_with("On it — "),
        "the follow-up trigger replies with the launch cue: {}",
        comments[1].2
    );
    assert!(
        comments[1].2.contains(&format!("/s/{fresh_id}")),
        "the reply links the fresh session: {}",
        comments[1].2
    );

    ts.client
        .delete(&format!("/api/sessions/{fresh_id}"))
        .await
        .unwrap();
}

/// A comment on a **pull request** attaches the session's worktree to the PR's own
/// head branch, so the agent's commits land on the PR.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_trigger_attaches_to_pr_head_branch() {
    let (ts, fake) = boot().await;
    let _remotes = prepare_repo(&ts).await;
    // The mock resolves PR #7's head to the `feature` branch the fixture created.
    *fake.pr_head.lock().unwrap() = Some(loom::github_trigger::PrHead {
        head_ref: "feature".to_string(),
        cross_repo: false,
    });

    let body = trigger_body_pr("rjpower", 7, "@loom fix the failing tests");
    assert_eq!(
        post(&ts, "d-pr", Some(sign(SECRET, &body)), &body)
            .await
            .status(),
        200
    );
    wait_for_sessions(&ts, 1).await;

    let sessions = ts.client.get("/api/sessions").await.unwrap();
    let sessions = sessions.as_array().unwrap();
    assert_eq!(sessions.len(), 1, "the PR trigger launched one session");
    assert_eq!(
        sessions[0]["branch"]["branch"].as_str(),
        Some("feature"),
        "the session is attached to the PR's head branch, not a fresh one"
    );

    let id = sessions[0]["id"].as_str().unwrap().to_string();
    ts.client
        .delete(&format!("/api/sessions/{id}"))
        .await
        .unwrap();
}
