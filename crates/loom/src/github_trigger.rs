//! The inbound GitHub trigger: a webhook that turns an `@loom work on this`
//! issue comment into a session and replies with its URL (shared-loom design
//! §6.3). This is the **untrusted-input boundary** — the receiver is exposed to
//! the internet, so every step here is a gate:
//!
//! 1. **Authenticate the delivery cryptographically.** GitHub signs each
//!    delivery with HMAC-SHA256 over the raw body; [`verify_signature`] checks it
//!    with a constant-time compare. A request without a valid signature is
//!    rejected before its body is even parsed.
//! 2. **Dedupe** on the `X-GitHub-Delivery` GUID ([`record_delivery`]) so a
//!    replayed or GitHub-retried delivery never launches a second session.
//! 3. **Filter** to `issue_comment`/`created`, ignoring the bot's own comments.
//! 4. **Parse** a fixed command prefix ([`is_trigger`]) — no free-text in v1.
//! 5. **Authorize the commenter** ([`authorize`]): a known loom operator, or
//!    someone with write/admin on the repo (checked via the GitHub API). Anyone
//!    else is silently ignored; a per-repo rate limit blunts spam.
//!
//! The HTTP glue that sequences these (and then creates the session + replies)
//! lives in [`crate::web::github_webhook`], which has access to the session
//! create path; the security primitives live here so they can be unit-tested in
//! isolation. External GitHub calls (the permission check and the reply) go
//! through the [`GithubApi`] gateway so a test can substitute a fake for `gh`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;
use tokio::process::Command;

use crate::auth;
use crate::db::Db;
use weaver_core::config;

type HmacSha256 = Hmac<Sha256>;

/// The settings key (read via env-or-DB) holding the webhook's shared secret.
/// Kept **out** of the settings registry, like the OAuth client secret, so it is
/// never returned by `GET /api/settings`; set it through the environment.
pub const WEBHOOK_SECRET_KEY: &str = "github.webhook_secret";
/// The settings key for the optional bot login whose own comments are ignored.
pub const BOT_LOGIN_KEY: &str = "github.bot_login";

/// Read `LOOM_GITHUB_WEBHOOK_SECRET`, else the `github.webhook_secret` setting.
/// Empty means the webhook is **not configured** — the receiver then rejects
/// every delivery (it cannot verify a signature without it).
pub async fn webhook_secret(db: &Db) -> String {
    env_or_setting(db, "LOOM_GITHUB_WEBHOOK_SECRET", WEBHOOK_SECRET_KEY).await
}

/// The trigger phrase a comment must begin with, from the
/// `github.trigger_phrase` setting (default `@loom work on this`).
pub async fn trigger_phrase(db: &Db) -> String {
    let phrase = config::get_or(
        db,
        "github.trigger_phrase",
        config::DEFAULT_GITHUB_TRIGGER_PHRASE,
    )
    .await
    .trim()
    .to_string();
    if phrase.is_empty() {
        config::DEFAULT_GITHUB_TRIGGER_PHRASE.to_string()
    } else {
        phrase
    }
}

/// The bot's own GitHub login, whose comments are ignored to prevent a
/// self-trigger loop. `LOOM_GITHUB_BOT_LOGIN`, else the `github.bot_login`
/// setting; `None` when unset (the command-prefix filter is the real guard, so
/// this is defence in depth and optional).
pub async fn bot_login(db: &Db) -> Option<String> {
    let login = env_or_setting(db, "LOOM_GITHUB_BOT_LOGIN", BOT_LOGIN_KEY).await;
    let login = login.trim();
    (!login.is_empty()).then(|| login.to_string())
}

/// `env`, else the `key` setting; empty when neither is set. (Mirrors the OAuth
/// secret resolution in [`crate::auth`], kept private to each module.)
async fn env_or_setting(db: &Db, env: &str, key: &str) -> String {
    if let Ok(v) = std::env::var(env) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    config::get(db, key).await.unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Signature verification — the cryptographic authentication of every delivery.
// ---------------------------------------------------------------------------

/// Verify GitHub's `X-Hub-Signature-256` over the **raw** request body. The
/// header is `sha256=<hex>`; we recompute HMAC-SHA256 of `body` keyed by
/// `secret` and compare in constant time ([`Mac::verify_slice`]). Returns
/// `false` — never panicking — for an empty secret (not configured), a missing
/// or malformed header, or any mismatch.
///
/// `body` MUST be the bytes exactly as received: the signature is over the wire
/// bytes, so re-serializing a parsed JSON value would change the digest.
pub fn verify_signature(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    if secret.is_empty() {
        return false;
    }
    let Some(header) = header else {
        return false;
    };
    let Some(hex_sig) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(provided) = hex::decode(hex_sig.trim()) else {
        return false;
    };
    // `new_from_slice` only errors on a key length HMAC can't take; SHA-256's
    // block construction accepts any key, so this never fails for a real secret.
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&provided).is_ok()
}

// ---------------------------------------------------------------------------
// Command + payload parsing.
// ---------------------------------------------------------------------------

/// Whether `body` begins with the trigger `phrase`, ignoring leading whitespace
/// and case. The match is anchored to the **start** so a quote of an earlier
/// comment, or the phrase buried mid-text, does not trigger.
pub fn is_trigger(body: &str, phrase: &str) -> bool {
    let body = body.trim_start().to_lowercase();
    let phrase = phrase.trim().to_lowercase();
    !phrase.is_empty() && body.starts_with(&phrase)
}

/// The `issue_comment` webhook payload, narrowed to the fields the trigger uses.
/// Deserialized from the verified raw body, so these values are trusted (they
/// came from GitHub, not the caller).
#[derive(Debug, Deserialize)]
pub struct IssueCommentEvent {
    /// `created` | `edited` | `deleted` — only `created` is acted on.
    pub action: String,
    pub issue: IssuePayload,
    pub comment: CommentPayload,
    pub repository: RepoPayload,
}

#[derive(Debug, Deserialize)]
pub struct IssuePayload {
    pub number: i64,
    #[serde(default)]
    pub title: String,
    /// GitHub sends `null` for an empty issue body, so this is an `Option`.
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CommentPayload {
    #[serde(default)]
    pub body: String,
    pub user: UserPayload,
}

#[derive(Debug, Deserialize)]
pub struct UserPayload {
    #[serde(default)]
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoPayload {
    /// `owner/name`.
    pub full_name: String,
}

impl IssueCommentEvent {
    /// Parse an `issue_comment` payload from the raw (already signature-verified)
    /// body.
    pub fn parse(body: &[u8]) -> Result<Self> {
        serde_json::from_slice(body).context("parsing issue_comment payload")
    }

    /// The seed text for the session goal: the issue title, plus its body when
    /// present.
    pub fn goal_seed(&self) -> String {
        match self.issue.body.as_deref().map(str::trim) {
            Some(b) if !b.is_empty() => format!("{}\n\n{}", self.issue.title, b),
            _ => self.issue.title.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Idempotency.
// ---------------------------------------------------------------------------

/// Record a delivery GUID, returning `true` only the **first** time it is seen.
/// A repeat (a replay, or a GitHub retry of a delivery we already handled)
/// returns `false`, so the caller treats it as a no-op.
pub async fn record_delivery(db: &Db, delivery_id: &str) -> Result<bool> {
    let res = sqlx::query("INSERT OR IGNORE INTO processed_deliveries (delivery_id) VALUES (?)")
        .bind(delivery_id)
        .execute(db)
        .await
        .context("recording webhook delivery")?;
    Ok(res.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// Commenter authorization — the untrusted boundary.
// ---------------------------------------------------------------------------

/// A GitHub login is `[A-Za-z0-9-]` (no leading/trailing hyphen, but we only
/// need the charset). Reject anything else before interpolating it into a
/// `gh api` path, and treat it as unauthorized.
fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether `login` may trigger a session on `owner/name`. Authorized iff the
/// login is a known loom operator (on the allowlist, checked with no GitHub
/// call), **or** has write/admin permission on the repo (checked via the
/// `GithubApi` gateway). Fails **closed**: a malformed login, an unknown user,
/// read/none permission, or any gateway error all deny.
pub async fn authorize(db: &Db, gh: &dyn GithubApi, owner: &str, name: &str, login: &str) -> bool {
    if !valid_login(login) {
        tracing::warn!(login, "rejecting trigger from a malformed GitHub login");
        return false;
    }
    // A known loom operator (their GitHub login is on the allowlist) is trusted
    // without spending a GitHub API call.
    match auth::user_by_github(db, login).await {
        Ok(Some(_)) => return true,
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, login, "loom-user lookup failed; falling back to repo permission")
        }
    }
    // Otherwise require write/admin on the repo itself.
    match gh.collaborator_permission(owner, name, login).await {
        Ok(perm) => {
            let ok = matches!(perm.as_str(), "admin" | "maintain" | "write");
            if !ok {
                tracing::info!(
                    login,
                    owner,
                    name,
                    perm,
                    "trigger denied: insufficient repo permission"
                );
            }
            ok
        }
        Err(e) => {
            // Most often a 404 (the login is not a collaborator); also any gh
            // failure. Either way, deny.
            tracing::info!(error = %e, login, owner, name, "trigger denied: permission check failed");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Per-repo rate limit — a simple sliding window to blunt issue-comment spam.
// ---------------------------------------------------------------------------

/// The window over which [`GithubTrigger::check_rate_limit`] counts triggers.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// The most triggers a single repo may fire within [`RATE_WINDOW`] before
/// further ones are dropped. Generous enough that ordinary use never trips it;
/// low enough that a comment flood cannot fan out into unbounded API calls and
/// session launches. The trade-off (shared-loom design §6.3): a spammer can
/// exhaust a repo's budget and briefly lock out legitimate triggers on it.
const RATE_MAX: usize = 20;

// ---------------------------------------------------------------------------
// The GitHub gateway — the two external operations the trigger performs.
// ---------------------------------------------------------------------------

/// The GitHub operations the trigger needs, behind a trait so the `gh`-backed
/// implementation ([`GhCli`]) and a test fake are interchangeable. Both use the
/// ambient `GH_TOKEN`; replacing `gh` with GitHub-App installation tokens is the
/// planned hardening (design §6.3).
#[async_trait::async_trait]
pub trait GithubApi: Send + Sync {
    /// The permission `login` has on `owner/name` — `admin` | `maintain` |
    /// `write` | `read` | `none`. Errors (e.g. a 404 for a non-collaborator)
    /// propagate so the caller can fail closed.
    async fn collaborator_permission(&self, owner: &str, name: &str, login: &str)
        -> Result<String>;
    /// Post a comment on `issue` of `repo` (`owner/name`).
    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<()>;
}

/// The production [`GithubApi`]: shells out to the `gh` CLI with the ambient
/// `GH_TOKEN`.
pub struct GhCli;

#[async_trait::async_trait]
impl GithubApi for GhCli {
    async fn collaborator_permission(
        &self,
        owner: &str,
        name: &str,
        login: &str,
    ) -> Result<String> {
        let path = format!("repos/{owner}/{name}/collaborators/{login}/permission");
        gh_capture(&["api", &path, "-q", ".permission"]).await
    }

    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<()> {
        let number = issue.to_string();
        gh_capture(&["issue", "comment", &number, "--repo", repo, "--body", body])
            .await
            .map(|_| ())
    }
}

/// Run `gh args…` (no repo working dir — every call passes `--repo` or a full
/// API path), returning trimmed stdout. A non-zero exit is an error carrying the
/// trimmed stderr.
async fn gh_capture(args: &[&str]) -> Result<String> {
    tracing::debug!(args = %args.join(" "), "running gh (trigger)");
    let out = Command::new("gh")
        .args(args)
        .output()
        .await
        .context("failed to spawn gh (is the GitHub CLI installed?)")?;
    if !out.status.success() {
        bail!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Shared trigger state held on [`crate::web::AppState`]: the GitHub gateway and
/// the per-repo rate limiter. One instance per server; the gateway is an `Arc`
/// so a test can install a fake.
pub struct GithubTrigger {
    gh: Arc<dyn GithubApi>,
    /// The GitHub **App** client, when production-built: the same object the
    /// `gh` gateway points at, kept concretely so the webhook can query
    /// installations (the implicit allowlist, §6.3). `None` when a test installs
    /// a bare [`GithubApi`] fake via [`with_gateway`](Self::with_gateway).
    app: Option<Arc<crate::github_app::GithubApp>>,
    /// Per-repo trigger timestamps, pruned to [`RATE_WINDOW`] on each check.
    limiter: Mutex<HashMap<String, Vec<Instant>>>,
}

impl GithubTrigger {
    /// The production trigger: a GitHub **App** gateway that mints short-lived
    /// installation tokens for the permission check and reply, falling back to
    /// the `gh` CLI (ambient `GH_TOKEN`) when the App is unconfigured.
    pub fn production(db: crate::db::Db) -> Arc<Self> {
        let app = Arc::new(crate::github_app::GithubApp::new(db));
        Arc::new(Self {
            gh: app.clone(),
            app: Some(app),
            limiter: Mutex::new(HashMap::new()),
        })
    }

    /// A trigger backed by an arbitrary [`GithubApi`] — the seam tests use to
    /// substitute a fake for `gh`. No App handle, so the installation-allowlist
    /// step is a no-op.
    pub fn with_gateway(gh: Arc<dyn GithubApi>) -> Arc<Self> {
        Arc::new(Self {
            gh,
            app: None,
            limiter: Mutex::new(HashMap::new()),
        })
    }

    /// The GitHub gateway, for the permission check and the reply.
    pub fn gh(&self) -> &dyn GithubApi {
        self.gh.as_ref()
    }

    /// The GitHub App client, when one is configured on this trigger — the
    /// webhook uses it to treat an App-installed repo as implicitly allowlisted.
    pub fn app(&self) -> Option<&crate::github_app::GithubApp> {
        self.app.as_deref()
    }

    /// Record a trigger attempt for `repo` and report whether it is within the
    /// rate budget. Returns `false` once a repo has fired [`RATE_MAX`] triggers
    /// inside [`RATE_WINDOW`]; the dropped attempt is the caller's no-op.
    pub fn check_rate_limit(&self, repo: &str) -> bool {
        let now = Instant::now();
        let mut map = self.limiter.lock().expect("rate-limiter mutex poisoned");
        let hits = map.entry(repo.to_string()).or_default();
        hits.retain(|t| now.duration_since(*t) < RATE_WINDOW);
        if hits.len() >= RATE_MAX {
            return false;
        }
        hits.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compute the hex `X-Hub-Signature-256` value a real GitHub delivery would
    /// carry for `(secret, body)`.
    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn verify_signature_accepts_a_correct_signature() {
        let secret = "s3cr3t";
        let body = br#"{"action":"created"}"#;
        let header = sign(secret, body);
        assert!(verify_signature(secret, body, Some(&header)));
    }

    #[test]
    fn verify_signature_rejects_tampering_and_bad_input() {
        let secret = "s3cr3t";
        let body = br#"{"action":"created"}"#;
        let good = sign(secret, body);

        // Wrong secret.
        assert!(!verify_signature("other", body, Some(&good)));
        // Body changed after signing.
        assert!(!verify_signature(
            secret,
            br#"{"action":"deleted"}"#,
            Some(&good)
        ));
        // Missing header.
        assert!(!verify_signature(secret, body, None));
        // Wrong algorithm prefix / malformed header.
        assert!(!verify_signature(secret, body, Some("sha1=deadbeef")));
        assert!(!verify_signature(secret, body, Some(&good[7..]))); // no "sha256=" prefix
        assert!(!verify_signature(secret, body, Some("sha256=nothex")));
        // An empty secret (unconfigured) never verifies, even with a header that
        // matches the empty-key HMAC.
        let empty_key_sig = sign("", body);
        assert!(!verify_signature("", body, Some(&empty_key_sig)));
    }

    #[test]
    fn is_trigger_anchors_to_the_start_ignoring_case_and_lead_space() {
        let phrase = "@loom work on this";
        assert!(is_trigger("@loom work on this issue please", phrase));
        assert!(is_trigger("  @LOOM Work On This", phrase));
        assert!(is_trigger("@loom work on this", phrase));
        // Not at the start → ignored (e.g. quoting someone else).
        assert!(!is_trigger("> @loom work on this", phrase));
        assert!(!is_trigger("please @loom work on this", phrase));
        assert!(!is_trigger("just a normal comment", phrase));
        // An empty phrase never matches (guards a misconfigured setting).
        assert!(!is_trigger("anything", ""));
    }

    #[test]
    fn parse_extracts_fields_and_seeds_the_goal() {
        let raw = br#"{
            "action": "created",
            "issue": {"number": 7, "title": "Fix the bug", "body": "It crashes"},
            "comment": {"body": "@loom work on this", "user": {"login": "alice"}},
            "repository": {"full_name": "acme/widgets"}
        }"#;
        let ev = IssueCommentEvent::parse(raw).unwrap();
        assert_eq!(ev.action, "created");
        assert_eq!(ev.issue.number, 7);
        assert_eq!(ev.comment.user.login, "alice");
        assert_eq!(ev.repository.full_name, "acme/widgets");
        assert_eq!(ev.goal_seed(), "Fix the bug\n\nIt crashes");

        // A null issue body is tolerated; the seed falls back to the title.
        let raw = br#"{
            "action": "created",
            "issue": {"number": 1, "title": "Title only", "body": null},
            "comment": {"body": "@loom work on this", "user": {"login": "bob"}},
            "repository": {"full_name": "acme/widgets"}
        }"#;
        let ev = IssueCommentEvent::parse(raw).unwrap();
        assert_eq!(ev.goal_seed(), "Title only");
    }

    #[test]
    fn valid_login_rejects_path_tricks() {
        assert!(valid_login("octocat"));
        assert!(valid_login("a-b-c"));
        assert!(!valid_login(""));
        assert!(!valid_login("../etc"));
        assert!(!valid_login("a/b"));
        assert!(!valid_login("has space"));
        assert!(!valid_login(&"x".repeat(40)));
    }

    #[tokio::test]
    async fn record_delivery_is_idempotent() {
        let db = crate::db::connect_in_memory().await.unwrap();
        assert!(
            record_delivery(&db, "guid-1").await.unwrap(),
            "first sighting is new"
        );
        assert!(
            !record_delivery(&db, "guid-1").await.unwrap(),
            "replay is a no-op"
        );
        assert!(
            record_delivery(&db, "guid-2").await.unwrap(),
            "a different delivery is new"
        );
    }

    /// A fake gateway recording the permission it should report and any comments
    /// posted, for the authorization and (integration) reply paths.
    #[derive(Default)]
    struct FakeGh {
        permission: Mutex<String>,
        fail_permission: Mutex<bool>,
        comments: Mutex<Vec<(String, i64, String)>>,
    }

    #[async_trait::async_trait]
    impl GithubApi for FakeGh {
        async fn collaborator_permission(&self, _o: &str, _n: &str, _l: &str) -> Result<String> {
            if *self.fail_permission.lock().unwrap() {
                bail!("simulated 404");
            }
            Ok(self.permission.lock().unwrap().clone())
        }
        async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<()> {
            self.comments
                .lock()
                .unwrap()
                .push((repo.to_string(), issue, body.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn authorize_trusts_loom_users_and_repo_writers_only() {
        let db = crate::db::connect_in_memory().await.unwrap();
        // A known loom operator is trusted with no GitHub call.
        auth::add_user(&db, "alice", Some("alice-gh"), None)
            .await
            .unwrap();
        let gh = FakeGh::default();
        *gh.permission.lock().unwrap() = "none".to_string();
        assert!(authorize(&db, &gh, "acme", "widgets", "alice-gh").await);

        // A stranger with write/admin is allowed; with read/none is denied.
        for (perm, allowed) in [
            ("admin", true),
            ("write", true),
            ("read", false),
            ("none", false),
        ] {
            *gh.permission.lock().unwrap() = perm.to_string();
            assert_eq!(
                authorize(&db, &gh, "acme", "widgets", "stranger").await,
                allowed,
                "permission {perm} should be allowed={allowed}"
            );
        }

        // A failed permission check fails closed.
        *gh.fail_permission.lock().unwrap() = true;
        assert!(!authorize(&db, &gh, "acme", "widgets", "stranger").await);

        // A malformed login is denied before any call.
        assert!(!authorize(&db, &gh, "acme", "widgets", "../etc").await);
    }

    #[test]
    fn rate_limit_caps_a_repo_then_lets_others_through() {
        let t = GithubTrigger::with_gateway(Arc::new(FakeGh::default()));
        for _ in 0..RATE_MAX {
            assert!(t.check_rate_limit("acme/widgets"));
        }
        // The next one over the budget is dropped...
        assert!(!t.check_rate_limit("acme/widgets"));
        // ...but a different repo has its own budget.
        assert!(t.check_rate_limit("acme/other"));
    }
}
