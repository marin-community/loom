//! The inbound GitHub trigger: a webhook that turns an `@loom` issue comment
//! into a session and replies with its URL (shared-loom design
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
//! 4. **Match** the trigger phrase ([`is_trigger`]) — a standalone mention
//!    anywhere in the comment's prose, quotes and code excluded.
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

/// The phrase that tags loom into a comment ([`is_trigger`]), from the
/// `github.trigger_phrase` setting (default `@loom`).
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
/// setting; `None` when unset.
///
/// Optional, but worth setting: loom's own replies never carry the phrase, and
/// [`is_trigger`] ignores quoted text, so the common loops are already closed —
/// but a session that replies on its thread and *mentions* the phrase in prose
/// would tag itself. [`authorize`] is the backstop (the bot identity is not
/// normally an approved user); this closes it a step earlier.
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

/// Whether `body` mentions the trigger `phrase` — matched case-insensitively
/// **anywhere** in the comment's prose, because people tag a bot mid-sentence as
/// readily as they open with it: `please @loom rebase this` tags as surely as
/// `@loom rebase this`.
///
/// *Prose* is the load-bearing word. Two narrow exclusions keep "anywhere" from
/// meaning "in text that isn't addressing the bot":
///
/// - **Quotes** ([`mentionable_text`] drops `>` lines). GitHub's quote-reply
///   pastes the comment being answered verbatim, so a plain substring match
///   would re-fire on every round of a thread.
/// - **Code**, fenced or inline. Talking *about* the trigger — ``the phrase is
///   `@loom` `` — is not using it.
///
/// The mention must also stand alone ([`mentions`]): `@loomer`, `me@loom.dev`,
/// and `@loom-bot` do not tag `@loom`.
///
/// An empty phrase never matches, guarding a misconfigured setting.
pub fn is_trigger(body: &str, phrase: &str) -> bool {
    let phrase = phrase.trim().to_lowercase();
    if phrase.is_empty() {
        return false;
    }
    mentions(&mentionable_text(body).to_lowercase(), &phrase)
}

/// Whether `text` contains `phrase` bounded by non-word characters on both
/// sides. Both arguments must already be lowercased.
///
/// `-` counts as a word character because a GitHub login is `[A-Za-z0-9-]`:
/// `@loom-bot` names a *different* account, not `@loom` with punctuation after
/// it. Ordinary trailing punctuation (`@loom!`) is not a word character, so it
/// still tags.
fn mentions(text: &str, phrase: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut from = 0;
    while let Some(rel) = text[from..].find(phrase) {
        let start = from + rel;
        let end = start + phrase.len();
        if !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word)
        {
            return true;
        }
        // This hit was glued to a word; a later one may still stand alone.
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// `body` reduced to the text a mention can live in: blockquote lines and code
/// (fenced or inline) removed. Each removal leaves whitespace behind, so cutting
/// a span never fuses its neighbours into one word.
fn mentionable_text(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        match (fence, fence_marker(trimmed)) {
            // Per CommonMark a block closes only on its own character, at least
            // as long as the fence that opened it — so a ```` block can hold a
            // ``` one (and a ``` block a ~~~ one) without ending early.
            (Some((open, len)), Some((c, run))) if c == open && run >= len => fence = None,
            (Some(_), _) => {}
            (None, Some(marker)) => fence = Some(marker),
            (None, None) if trimmed.starts_with('>') => {}
            (None, None) => {
                push_without_inline_code(line, &mut out);
                out.push('\n');
            }
        }
    }
    out
}

/// The character and length of a ```` ``` ````/`~~~` code-fence line (three or
/// more of either), else `None`.
fn fence_marker(trimmed: &str) -> Option<(char, usize)> {
    let mut chars = trimmed.chars();
    let c = chars.next().filter(|&c| c == '`' || c == '~')?;
    let run = 1 + chars.take_while(|&x| x == c).count();
    (run >= 3).then_some((c, run))
}

/// Append `line` to `out` with its inline-code spans blanked. A span opens on a
/// run of backticks and closes on the next run of the *same* length, per
/// CommonMark — so ``` ``@loom`` ``` is code, not a mention. An unclosed run is
/// literal text: only the backticks go, and the rest of the line stays prose.
fn push_without_inline_code(line: &str, out: &mut String) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let run = backtick_run(&chars, i);
        out.push(' ');
        i = closing_run(&chars, i + run, run).unwrap_or(i + run);
    }
}

/// How many backticks run consecutively from `i`.
fn backtick_run(chars: &[char], i: usize) -> usize {
    chars[i..].iter().take_while(|&&c| c == '`').count()
}

/// The index just past the first run of *exactly* `run` backticks at or after
/// `from`, or `None` when the span is never closed.
fn closing_run(chars: &[char], from: usize, run: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let r = backtick_run(chars, i);
        if r == run {
            return Some(i + r);
        }
        i += r;
    }
    None
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
    /// Present (a `{ url, … }` object) only when the comment is on a **pull
    /// request** — GitHub reuses the `issue_comment` event for both. Its mere
    /// presence is the PR/issue discriminant; the fields inside are unused.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

impl IssuePayload {
    /// Whether this comment is on a pull request (vs a plain issue).
    pub fn is_pr(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Debug, Deserialize)]
pub struct CommentPayload {
    /// The comment's REST id — the handle a reaction acknowledges it by.
    /// Defaults to 0 (treated as "unknown") if GitHub ever omits it.
    #[serde(default)]
    pub id: i64,
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

/// A GitHub login is `[A-Za-z0-9-]`, at most 39 chars. We only enforce the
/// charset and length — enough to keep junk out and to reject a login before it
/// reaches a store lookup; GitHub is the authority on whether the account exists.
pub fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether `login` may trigger a session. Authorized iff the login is an
/// **approved loom user** — a row in the `users` table, the *same* allowlist that
/// gates signing in to the app (checked with no GitHub call). Fails **closed**: a
/// malformed login, an unknown user, or a store error all deny.
///
/// One people-allowlist governs both surfaces: whoever may sign in to loom may
/// also trigger a session by commenting, and no one else — in particular, having
/// write access to the repo is *not* by itself a grant. (Extension point: a
/// future org-scoped rule such as "admins of org X" would be evaluated here,
/// consulting the GitHub API; deliberately not implemented yet.)
pub async fn authorize(db: &Db, login: &str) -> bool {
    if !valid_login(login) {
        tracing::warn!(login, "rejecting trigger from a malformed GitHub login");
        return false;
    }
    match auth::user_by_github(db, login).await {
        Ok(Some(_)) => true,
        Ok(None) => {
            tracing::info!(login, "trigger denied: not an approved loom user");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, login, "trigger denied: approved-user lookup failed");
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

/// A pull request's head branch — where its commits live — plus whether that
/// branch is in a fork (`cross_repo`). loom attaches a session's worktree to the
/// head branch so the agent's commits land on the PR directly; a cross-repo PR's
/// head lives in a fork loom can't push to, so those fall back to a fresh branch.
#[derive(Debug, Clone)]
pub struct PrHead {
    pub head_ref: String,
    pub cross_repo: bool,
}

/// The GitHub operations loom performs against a thread — posting and editing
/// comments, resolving a PR's head branch — behind a trait so the `gh`-backed
/// implementation ([`GhCli`]) and a test fake are interchangeable. The
/// production impl is the GitHub **App** (short-lived installation tokens); the
/// [`GhCli`] fallback uses the ambient `GH_TOKEN`.
#[async_trait::async_trait]
pub trait GithubApi: Send + Sync {
    /// Post a comment on `issue` of `repo` (`owner/name`), returning the new
    /// comment's id — the handle
    /// [`update_issue_comment`](Self::update_issue_comment) edits it by.
    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<i64>;

    /// Replace the body of comment `comment_id` of `repo` (an id
    /// [`post_issue_comment`](Self::post_issue_comment) returned). `Ok(false)`
    /// means the comment no longer exists (someone deleted it) — the caller may
    /// post a fresh one; transient failures stay `Err` so they are never
    /// answered by a duplicate comment.
    async fn update_issue_comment(&self, repo: &str, comment_id: i64, body: &str) -> Result<bool>;

    /// Resolve pull request `number` of `repo` (`owner/name`) to its head branch.
    async fn pr_head(&self, repo: &str, number: i64) -> Result<PrHead>;

    /// React to comment `comment_id` of `repo` with `content` (a GitHub
    /// reaction name, e.g. `"eyes"`). The quiet acknowledgment: visible on the
    /// comment itself, no thread noise, no notification.
    async fn react_to_comment(&self, repo: &str, comment_id: i64, content: &str) -> Result<()>;

    /// Fetch the live state of issue-or-PR `number` of `repo` — the minimal
    /// snapshot `weaver issue show` renders beside the weaver ledger.
    async fn issue_state(&self, repo: &str, number: i64) -> Result<IssueState>;
}

/// A thread's live state as GitHub reports it: `open`/`closed`, its title, and
/// the ISO time of its last touch. Deliberately minimal — an agent that needs
/// the discussion itself reads it with `gh`.
#[derive(Debug, Clone)]
pub struct IssueState {
    pub state: String,
    pub title: String,
    pub updated_at: String,
}

/// The production [`GithubApi`]: shells out to the `gh` CLI with the ambient
/// `GH_TOKEN`.
pub struct GhCli;

#[async_trait::async_trait]
impl GithubApi for GhCli {
    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<i64> {
        // `gh api` rather than the `gh issue comment` porcelain: the raw REST
        // response carries the comment id (porcelain prints only an HTML URL),
        // and the same route serves issues and pull requests alike.
        let path = format!("repos/{repo}/issues/{issue}/comments");
        let field = format!("body={body}");
        let out = gh_capture(&["api", &path, "-f", &field]).await?;
        let v: serde_json::Value =
            serde_json::from_str(&out).context("parsing created-comment json")?;
        let id = v["id"]
            .as_i64()
            .context("created-comment json carries no id")?;
        tracing::info!(repo, issue, comment = id, "posted issue comment");
        Ok(id)
    }

    async fn update_issue_comment(&self, repo: &str, comment_id: i64, body: &str) -> Result<bool> {
        let path = format!("repos/{repo}/issues/comments/{comment_id}");
        let field = format!("body={body}");
        match gh_capture(&["api", "-X", "PATCH", &path, "-f", &field]).await {
            Ok(_) => {
                tracing::info!(repo, comment = comment_id, "updated issue comment");
                Ok(true)
            }
            // `gh api` reports "gh: Not Found (HTTP 404)" on stderr.
            Err(e) if e.to_string().contains("HTTP 404") => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn react_to_comment(&self, repo: &str, comment_id: i64, content: &str) -> Result<()> {
        let path = format!("repos/{repo}/issues/comments/{comment_id}/reactions");
        let field = format!("content={content}");
        gh_capture(&["api", "-X", "POST", &path, "-f", &field]).await?;
        tracing::info!(repo, comment = comment_id, content, "reacted to comment");
        Ok(())
    }

    async fn issue_state(&self, repo: &str, number: i64) -> Result<IssueState> {
        // The `issues` route serves PR threads too (state/title/updated_at are
        // thread-level), so one call covers both kinds of tracking link.
        let path = format!("repos/{repo}/issues/{number}");
        let out = gh_capture(&["api", &path]).await?;
        let v: serde_json::Value = serde_json::from_str(&out).context("parsing issue json")?;
        Ok(IssueState {
            state: v["state"].as_str().unwrap_or_default().to_string(),
            title: v["title"].as_str().unwrap_or_default().to_string(),
            updated_at: v["updated_at"].as_str().unwrap_or_default().to_string(),
        })
    }

    async fn pr_head(&self, repo: &str, number: i64) -> Result<PrHead> {
        let n = number.to_string();
        let out = gh_capture(&[
            "pr",
            "view",
            &n,
            "--repo",
            repo,
            "--json",
            "headRefName,isCrossRepository",
        ])
        .await?;
        #[derive(Deserialize)]
        struct View {
            #[serde(rename = "headRefName")]
            head_ref: String,
            #[serde(rename = "isCrossRepository")]
            cross_repo: bool,
        }
        let view: View = serde_json::from_str(&out).context("parsing `gh pr view` json")?;
        Ok(PrHead {
            head_ref: view.head_ref,
            cross_repo: view.cross_repo,
        })
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
    fn is_trigger_matches_a_mention_anywhere_ignoring_case() {
        let phrase = "@loom";
        assert!(is_trigger("@loom rebase onto main", phrase));
        assert!(is_trigger("  @LOOM Rebase Onto Main", phrase));
        // The point of the substring match: people tag mid-sentence.
        assert!(is_trigger("please @loom rebase this", phrase));
        assert!(is_trigger("nice catch!\n\ncc @loom", phrase));
        assert!(is_trigger("**@loom** — over to you", phrase));
        assert!(!is_trigger("just a normal comment", phrase));
        // An empty phrase never matches (guards a misconfigured setting).
        assert!(!is_trigger("anything", ""));
        // A multi-word phrase behaves the same way.
        let phrase = "@loom work on this";
        assert!(is_trigger("ok — @loom work on this, please", phrase));
        assert!(is_trigger("@loom work on this", phrase));
        assert!(!is_trigger("@loom work on something else", phrase));
    }

    #[test]
    fn is_trigger_ignores_quoted_and_code_text() {
        let phrase = "@loom";
        // GitHub's quote-reply pastes the comment being answered — matching it
        // would re-fire the bot on every round of a thread.
        assert!(!is_trigger(
            "> @loom rebase onto main\n\ndone, thanks",
            phrase
        ));
        // Talking *about* the trigger is not using it.
        assert!(!is_trigger("does `@loom` still work?", phrase));
        assert!(!is_trigger("does ``@loom`` still work?", phrase));
        assert!(!is_trigger(
            "```\n@loom rebase\n```\nthat's the syntax",
            phrase
        ));
        assert!(!is_trigger("~~~\n@loom rebase\n~~~", phrase));
        // A shorter inner fence doesn't close the block, so this is all code.
        assert!(!is_trigger("````\n```\n@loom rebase\n```\n````", phrase));
        // ...but a real mention alongside quoted or code text still fires.
        assert!(is_trigger(
            "> an earlier comment\n\n@loom take another pass",
            phrase
        ));
        assert!(is_trigger(
            "run `git rebase` first, then @loom verify",
            phrase
        ));
        assert!(is_trigger("```\ncode\n```\n\n@loom review this", phrase));
        // An unclosed backtick is literal text, not an open code span that
        // swallows the rest of the comment.
        assert!(is_trigger("weird ` backtick, @loom look", phrase));
    }

    #[test]
    fn is_trigger_requires_a_standalone_mention() {
        let phrase = "@loom";
        assert!(!is_trigger("ping @loomer about it", phrase));
        assert!(!is_trigger("mail me@loom.dev", phrase));
        // A different account, not @loom with a hyphen after it.
        assert!(!is_trigger("that's @loom-bot's job", phrase));
        // Ordinary punctuation is not part of the mention.
        assert!(is_trigger("over to you, @loom!", phrase));
        assert!(is_trigger("(@loom)", phrase));
        // A glued hit does not mask a real one later on.
        assert!(is_trigger("me@loom.dev — and @loom, take a look", phrase));
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

    /// A fake gateway recording any comments posted (the reply path).
    #[derive(Default)]
    struct FakeGh {
        comments: Mutex<Vec<(String, i64, String)>>,
    }

    #[async_trait::async_trait]
    impl GithubApi for FakeGh {
        async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<i64> {
            let mut comments = self.comments.lock().unwrap();
            comments.push((repo.to_string(), issue, body.to_string()));
            Ok(comments.len() as i64)
        }

        async fn update_issue_comment(
            &self,
            _repo: &str,
            _comment_id: i64,
            _body: &str,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn pr_head(&self, _repo: &str, _number: i64) -> Result<PrHead> {
            Ok(PrHead {
                head_ref: "feature".to_string(),
                cross_repo: false,
            })
        }

        async fn react_to_comment(
            &self,
            _repo: &str,
            _comment_id: i64,
            _content: &str,
        ) -> Result<()> {
            Ok(())
        }

        async fn issue_state(&self, _repo: &str, _number: i64) -> Result<IssueState> {
            Ok(IssueState {
                state: "open".to_string(),
                title: "t".to_string(),
                updated_at: "2026-07-18T00:00:00Z".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn authorize_trusts_approved_users_only() {
        let db = crate::db::connect_in_memory().await.unwrap();
        // An approved loom user (their GitHub login is on the `users` allowlist —
        // the same one that gates sign-in) may trigger, with no GitHub call.
        auth::add_user(&db, "alice", Some("alice-gh"), None)
            .await
            .unwrap();
        assert!(authorize(&db, "alice-gh").await);
        // Case-insensitive, like GitHub logins and the sign-in check.
        assert!(authorize(&db, "ALICE-GH").await);

        // Anyone not on the allowlist is denied — write access to the repo is not
        // itself a grant.
        assert!(!authorize(&db, "stranger").await);
        // A malformed login is denied before any lookup.
        assert!(!authorize(&db, "../etc").await);
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
