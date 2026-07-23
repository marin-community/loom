//! Optional GitHub integration via the `gh` CLI. All functions degrade
//! gracefully — callers treat errors as "GitHub unavailable".
//!
//! Two responsibilities live here:
//!
//! * **Issue seeding** ([`fetch_issue`], [`repo_slug`]) and PR opening
//!   ([`create_pr`]) — one-shot shell-outs used when a session is created.
//! * **PR status polling** ([`poll`], [`refresh`], [`fetch_pr`]) — the
//!   background loop that snapshots each active session's pull request (link,
//!   review decision, check rollup) into the `branch_github` table, and
//!   archives a session — closing the weaver issues it was working — once its PR
//!   merges. The snapshot rides along on `BranchView`; the dashboard renders it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::db::{now_iso, Db};
use crate::session::{self as session_mod, Session};
use crate::web::AppState;
use crate::{branch as branch_mod, config, events};
use weaver_core::branch::Branch;
use weaver_core::github::GithubStatus;
use weaver_core::tags;

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: String,
}

async fn gh(dir: &Path, args: &[&str]) -> Result<String> {
    tracing::debug!(args = %args.join(" "), dir = %dir.display(), "running gh");
    let out = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to spawn gh");
            e
        })
        .context("failed to spawn gh (is the GitHub CLI installed?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        tracing::warn!(
            args = %args.join(" "),
            code = out.status.code().unwrap_or(-1),
            stderr = %stderr.trim(),
            "gh command failed"
        );
        bail!("gh {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `owner/name` slug for the repository at `repo_root`.
pub async fn repo_slug(repo_root: &Path) -> Result<String> {
    gh(
        repo_root,
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ],
    )
    .await
}

/// Fetch an issue's title/body/url.
pub async fn fetch_issue(repo_root: &Path, number: i64) -> Result<Issue> {
    let json = gh(
        repo_root,
        &[
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "title,body,url",
        ],
    )
    .await?;
    serde_json::from_str(&json).context("parsing gh issue JSON")
}

/// Open a pull request from the workspace branch; returns the PR URL.
pub async fn create_pr(work_dir: &Path, base: &str, title: &str, body: &str) -> Result<String> {
    tracing::debug!(base, title, body_len = body.len(), "creating pull request");
    let url = gh(
        work_dir,
        &[
            "pr", "create", "--base", base, "--title", title, "--body", body,
        ],
    )
    .await?;
    tracing::info!(url = %url, base, "pull request created");
    Ok(url)
}

// ---------------------------------------------------------------------------
// PR status snapshots
//
// The latest pull-request snapshot loom found for a branch, persisted in the
// `branch_github` table (one row per branch) and served inside `BranchView`.
// The background `poll` loop keeps it fresh; `refresh` does one branch on
// demand. Everything degrades to "no snapshot" when `gh` is missing or the repo
// has no GitHub remote.
// ---------------------------------------------------------------------------

const POLL_TICK: Duration = Duration::from_secs(30);

/// Branch tag marking that loom has back-linked this branch's PR to its session
/// (posted the `…/s/{id}` comment). Set by the poll loop's back-link poster or,
/// for a `@loom`-triggered PR, by the trigger reply — so exactly one link lands.
/// Shared with [`crate::web`] (the trigger reply path).
pub const LINKED_TAG: &str = tags::GITHUB_LINKED_KEY;

// ---------------------------------------------------------------------------
// The status card
//
// A branch wired to a GitHub thread mirrors its `weaver status` trail onto one
// comment there — the trigger's "On it" reply, edited in place — so the people
// watching the issue or PR see the agent's progress without opening the
// dashboard. Edits notify no one: the card is the quiet channel; the agent
// still posts real comments when it needs a human.
// ---------------------------------------------------------------------------

/// Branch tag wiring a session to a GitHub thread — see
/// [`tags::GITHUB_KEY`] (the registry entry) for the contract.
pub const WIRED_TAG: &str = tags::GITHUB_KEY;

/// Branch tag holding the GitHub comment id of the status card — loom's
/// bookkeeping, the same shape as [`LINKED_TAG`] (the value is the handle).
/// See [`tags::GITHUB_COMMENT_KEY`].
pub const STATUS_COMMENT_TAG: &str = tags::GITHUB_COMMENT_KEY;

/// How many trail bullets the status card shows; older entries collapse into a
/// count line so a long session never turns the comment into a scroll.
const STATUS_CARD_CAP: usize = 15;

/// Keys loom stamps mechanically to track its own integration side-effects —
/// now centralized as [`tags::is_reserved_tag`] (both generic tag routes gate on
/// it, and Slack bookkeeping joins the same list). Re-exported here for the
/// GitHub call sites that reference it.
pub use tags::is_reserved_tag;

/// Parse a [`WIRED_TAG`] value — `owner/name#number` — into its slug and
/// thread number. `None` for anything else (the tag is free-form user input,
/// and the slug ends up in GitHub API paths): both segments must pass the same
/// charset gate as [`crate::repo::parse_slug`], and only the bare documented
/// form is accepted — a URL form parse_slug would reduce is rejected here so
/// the stored tag, the sync's API paths, and the comment bookkeeping note all
/// agree on one spelling.
pub fn parse_wiring(value: &str) -> Option<(String, i64)> {
    let (slug, number) = value.trim().split_once('#')?;
    let number: i64 = number.trim().parse().ok().filter(|n| *n > 0)?;
    let slug = slug.trim();
    let parsed = crate::repo::parse_slug(slug).ok()?;
    (parsed.slug() == slug).then(|| (parsed.slug(), number))
}

/// One rendered bullet of the status trail: the level's dot, the time, the
/// level name when loud, and the message. Events with nothing to say (a bare
/// `weaver status ok`) render `None`.
fn status_bullet(event: &weaver_core::events::Event) -> Option<String> {
    let value = event.data["value"].as_str().unwrap_or_default();
    let note = event.data["note"].as_str().unwrap_or_default().trim();
    let loud = !value.is_empty();
    if note.is_empty() && !loud {
        return None;
    }
    let icon = match value {
        "blocked" => "\u{1f534}",   // red circle
        "attention" => "\u{1f7e0}", // orange circle
        _ => "\u{1f7e2}",           // green circle
    };
    let when = chrono::DateTime::parse_from_rfc3339(&event.created_at)
        .map(|t| t.format("%b %e %H:%M").to_string())
        .unwrap_or_default();
    let mut line = format!("- {icon} `{when}Z`");
    if loud {
        line.push_str(&format!(" **{value}**"));
        if !note.is_empty() {
            line.push_str(" —");
        }
    }
    if !note.is_empty() {
        line.push_str(&format!(" {note}"));
    }
    Some(line)
}

/// Render the status card: the "On it" header linking the session, the
/// documents the agent has published (linked into the dashboard's artifact
/// viewer), then the trail of `weaver status` reports (oldest first, capped at
/// [`STATUS_CARD_CAP`]). Pure, so the format is unit-testable; `events` is the
/// branch's history oldest-first, filtered here to the agent's own attention
/// reports.
pub fn render_status_card(
    session_url: &str,
    artifacts: &[String],
    events: &[weaver_core::events::Event],
) -> String {
    let bullets: Vec<String> = events
        .iter()
        // The agent's own reports only: a manual tag edit in the UI records the
        // same event shape but must never read as agent progress on GitHub.
        .filter(|e| {
            e.kind == "tag" && e.data["key"] == tags::ATTENTION_KEY && e.data["by"] == "agent"
        })
        .filter_map(status_bullet)
        .collect();
    let mut body = format!("On it — {session_url}");
    if !artifacts.is_empty() {
        // Artifact names are agent-chosen free text: escape the Markdown label
        // and percent-encode the URL path segment, or a bracket/paren in a name
        // silently breaks the card's link syntax.
        use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
        const SEG: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'%')
            .add(b'/')
            .add(b'?')
            .add(b'(')
            .add(b')')
            .add(b'<')
            .add(b'>')
            .add(b'[')
            .add(b']')
            .add(b'\\')
            .add(b'`');
        let links: Vec<String> = artifacts
            .iter()
            .map(|name| {
                let label = name
                    .replace('\\', "\\\\")
                    .replace('[', "\\[")
                    .replace(']', "\\]");
                let path = utf8_percent_encode(name, SEG);
                format!("[{label}]({session_url}/artifacts/{path})")
            })
            .collect();
        body.push_str(&format!("\nDocs: {}", links.join(" · ")));
    }
    if bullets.is_empty() {
        return body;
    }
    body.push_str("\n\n");
    if bullets.len() > STATUS_CARD_CAP {
        let hidden = bullets.len() - STATUS_CARD_CAP;
        body.push_str(&format!("…{hidden} earlier updates\n"));
    }
    let shown = &bullets[bullets.len().saturating_sub(STATUS_CARD_CAP)..];
    body.push_str(&shown.join("\n"));
    body
}

/// One card sync at a time, process-wide: two racing status writes must never
/// both see "no tracked comment" and double-post. Coarse — every branch shares
/// it — which costs nothing at this fleet's write rate; each sync re-reads the
/// event history inside the lock, so whichever runs last renders the full
/// trail and stale renders can't stick.
static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Mirror a wired branch's status trail onto its GitHub thread: render the
/// card and edit the tracked comment in place — posting a fresh one the first
/// time, or again when someone deleted it. A no-op for unwired branches, and
/// best-effort everywhere: a GitHub hiccup logs and never surfaces to the
/// status write that spawned this. Transient failures retry — a terminal
/// status ("ready for review", a final `blocked`) has no later write to
/// self-heal through, so it must not be lost to one flaky call.
pub async fn sync_status_comment(state: AppState, branch_id: String) {
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(5 * 4u64.pow(attempt - 1))).await;
        }
        if sync_status_comment_once(&state, &branch_id).await {
            return;
        }
    }
    tracing::warn!(branch = %branch_id, "status card: giving up after retries");
}

/// One sync attempt. `true` means done — synced, or nothing to do (unwired, no
/// live session, no public base); `false` is a transient GitHub failure worth
/// retrying.
async fn sync_status_comment_once(state: &AppState, branch_id: &str) -> bool {
    let wired = match tags::get(&state.db, branch_id, WIRED_TAG).await {
        Ok(Some(tag)) => tag,
        Ok(None) => return true,
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "status card: reading wiring tag failed");
            return true;
        }
    };
    let Some((slug, number)) = parse_wiring(&wired.value) else {
        tracing::warn!(branch = %branch_id, value = %wired.value, "status card: unparsable `github` tag; expected owner/name#number");
        return true;
    };
    // Only a public base yields a link a GitHub reader can follow — same guard
    // as the PR back-link. Without one the card would advertise a loopback URL.
    let base = config::get(&state.db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if base.is_empty() {
        tracing::debug!(branch = %branch_id, "status card: no auth.base_url; skipping");
        return true;
    }
    let session = match session_mod::active_for_branch(&state.db, branch_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return true, // no live session — nothing is reporting status
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "status card: session lookup failed");
            return true;
        }
    };

    let _guard = SYNC_LOCK.lock().await;
    let mut events = match events::history(&state.db, branch_id, 500).await {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "status card: reading event history failed");
            return true;
        }
    };
    // The trail starts when the wiring did: statuses written before the agent
    // was told they'd be public stay private (hand-wiring an old session must
    // not retroactively publish its history).
    events.retain(|e| e.created_at >= wired.set_at);
    // The card links the agent's published documents (`goal` excluded — it
    // paraphrases the thread itself). Names only: full contents stay a
    // deliberate `gh` comment, posting one is the "please read this" act.
    let artifacts: Vec<String> = match branch_mod::get(&state.db, branch_id).await {
        Ok(Some(branch)) => {
            weaver_core::artifact::list_for_session(&state.db, &branch.repo_root, branch_id)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.name)
                .filter(|n| n != "goal")
                .collect()
        }
        _ => Vec::new(),
    };
    let body = render_status_card(
        &crate::web::session_url(&base, &session.id),
        &artifacts,
        &events,
    );

    // The tracked comment counts only while its note still names the current
    // wiring: a re-pointed `github` tag must get a fresh comment on the new
    // thread, never a PATCH of the old one.
    let wired_to = format!("{slug}#{number}");
    let tracked: Option<i64> = match tags::get(&state.db, branch_id, STATUS_COMMENT_TAG).await {
        Ok(tag) => tag
            .filter(|t| t.note == wired_to)
            .and_then(|t| t.value.parse().ok()),
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "status card: reading comment tag failed");
            return true;
        }
    };
    if let Some(comment_id) = tracked {
        match state
            .trigger
            .gh()
            .update_issue_comment(&slug, comment_id, &body)
            .await
        {
            Ok(true) => return true,
            Ok(false) => {
                tracing::info!(repo = %slug, comment = comment_id, "status card: comment gone; posting a fresh one");
            }
            Err(e) => {
                tracing::warn!(repo = %slug, comment = comment_id, error = %e, "status card: comment update failed");
                return false;
            }
        }
    }
    match state
        .trigger
        .gh()
        .post_issue_comment(&slug, number, &body)
        .await
    {
        Ok(comment_id) => {
            record_status_comment(&state.db, branch_id, &slug, number, comment_id).await;
            true
        }
        Err(e) => {
            tracing::warn!(repo = %slug, number, error = %e, "status card: posting comment failed");
            false
        }
    }
}

/// Stamp the [`STATUS_COMMENT_TAG`] bookkeeping tag after a card lands. The
/// note records the wiring the comment belongs to — [`sync_status_comment`]
/// trusts the comment id only while that note matches the current `github`
/// tag. Shared with the trigger reply path, which posts the card's first
/// revision.
pub async fn record_status_comment(
    db: &Db,
    branch_id: &str,
    slug: &str,
    number: i64,
    comment_id: i64,
) {
    tags::set(
        db,
        branch_id,
        STATUS_COMMENT_TAG,
        &comment_id.to_string(),
        &format!("{slug}#{number}"),
        "loom",
    )
    .await
    .ok();
}

/// The fields requested from `gh pr view --json`. Kept in one place so the parse
/// struct and the query can't drift.
const PR_FIELDS: &str =
    "number,url,state,title,isDraft,reviewDecision,mergeable,mergedAt,statusCheckRollup";

/// The shape of one `gh pr view --json` record. Internal — callers see
/// [`GithubStatus`].
#[derive(Debug, Deserialize)]
struct PrJson {
    number: i64,
    url: String,
    state: String,
    title: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "mergedAt", default)]
    merged_at: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Option<Vec<CheckJson>>,
}

/// One entry in `statusCheckRollup`. The array is a union of GitHub's CheckRun
/// (carries `status` + `conclusion`) and StatusContext (carries `state`); we
/// accept whichever fields are present.
#[derive(Debug, Deserialize)]
struct CheckJson {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl PrJson {
    fn into_status(self) -> GithubStatus {
        let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
        GithubStatus {
            pr_number: self.number,
            pr_url: self.url,
            pr_state: self.state,
            pr_title: self.title,
            is_draft: self.is_draft,
            review_decision: nonempty(self.review_decision),
            checks: rollup_checks(self.status_check_rollup.as_deref().unwrap_or(&[])),
            mergeable: nonempty(self.mergeable),
            merged_at: nonempty(self.merged_at),
            fetched_at: now_iso(),
        }
    }
}

/// Roll a PR's individual checks up to a single verdict, the way `gh pr checks`
/// does: any failure ⇒ `failing`, else anything still running ⇒ `pending`, else
/// `passing`. `None` when the PR has no checks at all.
fn rollup_checks(items: &[CheckJson]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut any_pending = false;
    let mut any_fail = false;
    for it in items {
        if let Some(status) = it.status.as_deref() {
            // CheckRun: only COMPLETED runs have a meaningful conclusion.
            if status != "COMPLETED" {
                any_pending = true;
                continue;
            }
            match it.conclusion.as_deref().unwrap_or("") {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
                "" => any_pending = true,
                _ => any_fail = true, // FAILURE / CANCELLED / TIMED_OUT / ACTION_REQUIRED / …
            }
        } else if let Some(state) = it.state.as_deref() {
            // StatusContext (legacy commit statuses).
            match state {
                "SUCCESS" => {}
                "PENDING" | "EXPECTED" => any_pending = true,
                _ => any_fail = true, // FAILURE / ERROR
            }
        }
    }
    Some(
        if any_fail {
            "failing"
        } else if any_pending {
            "pending"
        } else {
            "passing"
        }
        .to_string(),
    )
}

/// Whether a branch's check rollup just transitioned **into** failing: the new
/// snapshot is `failing` and the previously-stored value was not. The first time
/// a branch is seen (`prev_checks == None`) counts as a transition if it is
/// already failing, so a PR that is red on first sighting still announces once.
fn checks_went_red(prev_checks: Option<&str>, next: &GithubStatus) -> bool {
    next.checks.as_deref() == Some("failing") && prev_checks != Some("failing")
}

/// The mirror of [`checks_went_red`]: the rollup just transitioned **into**
/// passing. The first sighting of an already-green PR counts as a transition.
fn checks_went_green(prev_checks: Option<&str>, next: &GithubStatus) -> bool {
    next.checks.as_deref() == Some("passing") && prev_checks != Some("passing")
}

/// Whether the PR just became visible as **open**: now `OPEN` and previously
/// not (unseen, or reopened from `CLOSED`). Lets a watch act once when a
/// session's PR first appears, rather than re-checking every poll.
fn pr_just_opened(prev_state: Option<&str>, next: &GithubStatus) -> bool {
    next.pr_state == "OPEN" && prev_state != Some("OPEN")
}

/// Whether the PR just **merged**: now `MERGED` and previously not.
fn pr_just_merged(prev_state: Option<&str>, next: &GithubStatus) -> bool {
    next.pr_state == "MERGED" && prev_state != Some("MERGED")
}

/// Whether the review decision changed to a new non-null value (an approval, a
/// changes-requested, …). A decision dropping back to null (review no longer
/// required) is not announced — there is nothing to react to.
fn review_decision_changed(prev: Option<&GithubStatus>, next: &GithubStatus) -> bool {
    next.review_decision.is_some()
        && prev.map(|p| &p.review_decision) != Some(&next.review_decision)
}

/// Whether the `gh` CLI is usable on this machine. Probed once and cached — a
/// missing `gh` is the common "GitHub integration off" case and shouldn't cost a
/// process spawn on every poll.
pub async fn gh_available() -> bool {
    static AVAILABLE: OnceCell<bool> = OnceCell::const_new();
    *AVAILABLE
        .get_or_init(|| async {
            let ok = Command::new("gh")
                .arg("--version")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                tracing::info!("gh CLI detected — GitHub PR polling available");
            } else {
                tracing::info!("gh CLI not found — GitHub PR polling disabled");
            }
            ok
        })
        .await
}

/// Fetch one pull request by number (or another exact `gh pr view` selector).
/// This is used for a user-selected mapping and to follow an already-open PR
/// through its terminal transition.
pub async fn fetch_pr(
    repo_root: &Path,
    selector: &str,
    token: Option<&str>,
) -> Result<Option<GithubStatus>> {
    let mut cmd = Command::new("gh");
    cmd.args(["pr", "view", selector, "--json", PR_FIELDS])
        .current_dir(repo_root);
    // The poll loop runs in the loom process, which carries no ambient `gh` auth
    // (the operator's `GH_TOKEN` is session-scoped, not the server's). Without a
    // token `gh pr view` fails and every branch's PR status stays blank — starving
    // the pr-label / review-wait / archive-merged watches, which key off it.
    if let Some(token) = token {
        cmd.env("GH_TOKEN", token);
    }
    let out = cmd
        .output()
        .await
        .context("failed to spawn gh (is the GitHub CLI installed?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // gh exits non-zero when the branch has no PR; that's not an error.
        if stderr.to_lowercase().contains("no pull requests found")
            || stderr.to_lowercase().contains("no open pull requests")
        {
            return Ok(None);
        }
        bail!("gh pr view {selector} failed: {}", stderr.trim());
    }
    let raw: PrJson = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .context("parsing gh pr JSON")?;
    Ok(Some(raw.into_status()))
}

/// Discover the branch's current open PR. `gh pr view <branch>` may keep
/// returning an old closed/merged PR forever; the list query makes "current"
/// explicit and returns no result once the branch has no open PR.
pub async fn fetch_open_pr(
    repo_root: &Path,
    branch: &str,
    token: Option<&str>,
) -> Result<Option<GithubStatus>> {
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr", "list", "--head", branch, "--state", "open", "--limit", "1", "--json", PR_FIELDS,
    ])
    .current_dir(repo_root);
    if let Some(token) = token {
        cmd.env("GH_TOKEN", token);
    }
    let out = cmd
        .output()
        .await
        .context("failed to spawn gh (is the GitHub CLI installed?)")?;
    if !out.status.success() {
        bail!(
            "gh pr list --head {branch} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut rows: Vec<PrJson> =
        serde_json::from_slice(&out.stdout).context("parsing gh pr list JSON")?;
    Ok(rows.pop().map(PrJson::into_status))
}

/// The stored snapshot for a branch, if one has been fetched.
pub async fn get_status(db: &Db, branch_id: &str) -> Result<Option<GithubStatus>> {
    let row = sqlx::query_as::<_, GithubStatus>(
        "SELECT pr_number, pr_url, pr_state, pr_title, is_draft, review_decision,
                checks, mergeable, merged_at, fetched_at
         FROM branch_github WHERE branch_id = ?",
    )
    .bind(branch_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// The user-selected PR number for a branch. `None` keeps automatic discovery.
pub async fn get_mapping(db: &Db, branch_id: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT pr_number FROM branch_github_mapping WHERE branch_id = ?")
            .bind(branch_id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(number,)| number))
}

/// Pin a branch to one PR until the mapping is explicitly cleared or changed.
pub async fn set_mapping(db: &Db, branch_id: &str, number: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO branch_github_mapping (branch_id, pr_number) VALUES (?, ?)
         ON CONFLICT(branch_id) DO UPDATE SET pr_number = excluded.pr_number",
    )
    .bind(branch_id)
    .bind(number)
    .execute(db)
    .await?;
    Ok(())
}

/// Return a branch to automatic current-PR discovery.
pub async fn clear_mapping(db: &Db, branch_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM branch_github_mapping WHERE branch_id = ?")
        .bind(branch_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Forget a cached snapshot when automatic discovery says there is no current
/// PR. Leaving the row behind would make every view and watch consume stale PR
/// state indefinitely.
pub async fn clear_status(db: &Db, branch_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM branch_github WHERE branch_id = ?")
        .bind(branch_id)
        .execute(db)
        .await?;
    Ok(())
}

/// The branch name GitHub should use for automatic PR discovery.
///
/// A session's [`Branch`] is its durable loom identity, but an agent is free to
/// switch or rename the checked-out git branch after launch. Workflows such as
/// Marin's `fix-issue` skill do exactly that before opening an `agent/...` PR.
/// Prefer the worktree's live HEAD so the poller follows that PR; fall back to
/// the stored name when the worktree is missing, detached, or unreadable.
async fn discovery_branch(session: &Session, branch: &Branch) -> String {
    match crate::git::current_branch(Path::new(&session.work_dir)).await {
        Ok(current) if !current.trim().is_empty() && current != "HEAD" => {
            if current != branch.branch {
                tracing::debug!(
                    session = %session.id,
                    stored_branch = %branch.branch,
                    worktree_branch = %current,
                    "using worktree HEAD for GitHub PR discovery"
                );
            }
            current
        }
        _ => branch.branch.clone(),
    }
}

/// Persist (replacing) the snapshot for a branch.
pub async fn upsert_status(db: &Db, branch_id: &str, s: &GithubStatus) -> Result<()> {
    sqlx::query(
        "INSERT INTO branch_github
           (branch_id, pr_number, pr_url, pr_state, pr_title, is_draft,
            review_decision, checks, mergeable, merged_at, fetched_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(branch_id) DO UPDATE SET
           pr_number = excluded.pr_number, pr_url = excluded.pr_url,
           pr_state = excluded.pr_state, pr_title = excluded.pr_title,
           is_draft = excluded.is_draft, review_decision = excluded.review_decision,
           checks = excluded.checks, mergeable = excluded.mergeable,
           merged_at = excluded.merged_at, fetched_at = excluded.fetched_at",
    )
    .bind(branch_id)
    .bind(s.pr_number)
    .bind(&s.pr_url)
    .bind(&s.pr_state)
    .bind(&s.pr_title)
    .bind(s.is_draft)
    .bind(&s.review_decision)
    .bind(&s.checks)
    .bind(&s.mergeable)
    .bind(&s.merged_at)
    .bind(&s.fetched_at)
    .execute(db)
    .await?;
    Ok(())
}

/// Fetch a branch's PR, store the snapshot, announce a meaningful change on the
/// activity feed, and — when `archive_on_merge` is set and the PR has merged —
/// archive the still-live session and close the weaver issues it was working.
/// Returns the fresh snapshot, or `None` when the branch has no PR. The single
/// code path behind both the poller and the on-demand refresh endpoint.
pub async fn refresh(
    state: &AppState,
    session: &Session,
    branch: &Branch,
    archive_on_merge: bool,
) -> Result<Option<GithubStatus>> {
    // loom's own token for `gh` — the operator's `GH_TOKEN` from Settings →
    // Environment (the server process has no ambient GitHub auth of its own).
    let token = crate::agent_env::get(&state.db, "GH_TOKEN").await;
    let repo_root = PathBuf::from(&branch.repo_root);
    let previous = get_status(&state.db, &branch.id).await?;
    let mapping = get_mapping(&state.db, &branch.id).await?;
    let fetched = if let Some(number) = mapping {
        fetch_pr(&repo_root, &number.to_string(), token.as_deref()).await?
    } else {
        // Automatic mode always asks GitHub which open PR is current instead of
        // treating yesterday's cached number as authoritative. Only when the
        // branch has no open PR do we revisit the previous number once, to
        // preserve its close/merge transition before the snapshot is cleared.
        let head = discovery_branch(session, branch).await;
        let current = fetch_open_pr(&repo_root, &head, token.as_deref()).await?;
        if current.is_some() {
            current
        } else if previous.as_ref().is_some_and(|pr| pr.pr_state == "OPEN") {
            let number = previous.as_ref().expect("checked above").pr_number;
            fetch_pr(&repo_root, &number.to_string(), token.as_deref()).await?
        } else {
            None
        }
    };
    let snap = match fetched {
        Some(s) => s,
        None => {
            if previous.is_some() {
                clear_status(&state.db, &branch.id).await?;
                events::record(
                    &state.db,
                    &state.bus,
                    &branch.id,
                    "github",
                    json!({ "pr": null, "state": null }),
                )
                .await
                .ok();
            }
            return Ok(None);
        }
    };
    apply_snapshot(state, session, branch, &snap, archive_on_merge).await?;
    Ok(Some(snap))
}

/// Persist a freshly-fetched snapshot, announce a meaningful change on the
/// activity feed, and — when `archive_on_merge` is set — archive a still-live
/// session whose PR has merged and close the weaver issues that session claimed.
/// Split from [`refresh`] so the storage and merge-archive behaviour is testable
/// without invoking `gh`.
pub(crate) async fn apply_snapshot(
    state: &AppState,
    session: &Session,
    branch: &Branch,
    snap: &GithubStatus,
    archive_on_merge: bool,
) -> Result<()> {
    let prev = get_status(&state.db, &branch.id).await?;
    upsert_status(&state.db, &branch.id, snap).await?;
    let changed = prev.as_ref().map(GithubStatus::signature) != Some(snap.signature());
    if changed {
        events::record(
            &state.db,
            &state.bus,
            &branch.id,
            "github",
            snap.event_data(),
        )
        .await
        .ok();
    }

    // Edge-detect meaningful PR transitions and emit a one-shot event per
    // transition (compared against the *prior* stored snapshot, so each fires
    // once and not every poll while the condition persists). These are the
    // surfaces watches subscribe to via `pr.*` triggers — so a PR labeller wakes
    // on `pr.opened`, an archiver on `pr.merged`, a CI watcher on the check
    // edges — instead of polling the fleet on a timer.
    let prev_checks = prev.as_ref().and_then(|p| p.checks.as_deref());
    let prev_state = prev.as_ref().map(|p| p.pr_state.as_str());
    for (fire, kind, data) in [
        (
            pr_just_opened(prev_state, snap),
            "pr_opened",
            json!({ "pr": snap.pr_number }),
        ),
        (
            pr_just_merged(prev_state, snap),
            "pr_merged",
            json!({ "pr": snap.pr_number }),
        ),
        (
            checks_went_red(prev_checks, snap),
            "pr_red",
            json!({ "pr": snap.pr_number, "checks": "failing" }),
        ),
        (
            checks_went_green(prev_checks, snap),
            "pr_green",
            json!({ "pr": snap.pr_number, "checks": "passing" }),
        ),
        (
            review_decision_changed(prev.as_ref(), snap),
            "pr_review",
            json!({ "pr": snap.pr_number, "decision": snap.review_decision }),
        ),
    ] {
        if fire {
            events::record(&state.db, &state.bus, &branch.id, kind, data)
                .await
                .ok();
        }
    }

    // Back-link the PR to its loom session — a one-time comment posted when loom
    // first sees the PR open, so someone reading the GitHub thread can jump to the
    // live session. Gated on the open *transition* (not every poll while OPEN): a
    // repo where loom can't comment then fails once, rather than re-posting — and
    // logging a failure — on every 30s poll. Skipped once the branch is tagged
    // linked (here or by the `@loom` reply).
    if pr_just_opened(prev_state, snap) {
        maybe_post_backlink(state, session, branch, snap).await;
    }

    let recovered = tags::get(&state.db, &branch.id, tags::RECOVERED_KEY)
        .await?
        .is_some_and(|tag| tag.value == tags::RECOVERED_VALUE);
    if archive_on_merge
        && snap.pr_state == "MERGED"
        && !session_mod::is_terminal(&session.status)
        && !recovered
    {
        // Archive releases the branch's issue claims back to the backlog. Close
        // the shipped work first so the existing merge lifecycle still finds
        // those issues; archive then clears ownership from the closed rows too.
        close_claimed_issues(state, branch, snap.pr_number).await;
        // The merge is already on the record as a `github` event (above) and the
        // archive records a `status` event, so no extra log line is needed.
        match crate::web::archive(state, session, branch).await {
            Ok(_) => {
                tracing::info!(
                    branch = %branch.branch,
                    pr = snap.pr_number,
                    "archived session after PR merge"
                );
            }
            Err(e) => tracing::warn!(
                branch = %branch.branch,
                error = %e.message(),
                "archive-on-merge failed"
            ),
        }
    }
    Ok(())
}

/// Derive the `owner/name` slug for a managed clone from its on-disk root
/// (`<repos_dir>/owner/name`). `None` for a local-path repo (forked from `cwd`),
/// which has no managed slug — and may have no GitHub remote — so it's skipped.
fn slug_from_repo_root(repo_root: &str) -> Option<String> {
    let rest = Path::new(repo_root)
        .strip_prefix(crate::repo::repos_dir())
        .ok()?;
    let s = rest.to_string_lossy().replace('\\', "/");
    (s.matches('/').count() == 1 && !s.is_empty()).then_some(s)
}

/// Post a one-time comment on the branch's open PR linking back to its loom
/// session, unless the branch is already linked (the `@loom` reply tagged it).
/// Managed repos only — the App gateway needs the slug — and only when a public
/// base URL is configured, so the link resolves. Best-effort: a failure logs and
/// returns without setting the tag; the caller fires this only on the PR-open
/// transition, so a failure is not retried on every poll.
async fn maybe_post_backlink(
    state: &AppState,
    session: &Session,
    branch: &Branch,
    snap: &GithubStatus,
) {
    match tags::get(&state.db, &branch.id, LINKED_TAG).await {
        Ok(Some(_)) => return, // already linked
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(branch = %branch.branch, error = %e, "back-link: reading link tag failed");
            return;
        }
    }
    let Some(slug) = slug_from_repo_root(&branch.repo_root) else {
        return; // local-path repo: no managed slug
    };
    let base = crate::config::get(&state.db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if base.is_empty() {
        return; // no public URL configured → a link wouldn't resolve
    }
    let body = format!(
        "Working on this in loom: {}",
        crate::web::session_url(&base, &session.id)
    );
    if let Err(e) = state
        .trigger
        .gh()
        .post_issue_comment(&slug, snap.pr_number, &body)
        .await
    {
        tracing::warn!(repo = %slug, pr = snap.pr_number, error = %e, "back-link: posting comment failed");
        return;
    }
    tags::set(
        &state.db,
        &branch.id,
        LINKED_TAG,
        &session.id,
        "loom back-link comment posted",
        "loom",
    )
    .await
    .ok();
    tracing::info!(session = %session.id, repo = %slug, pr = snap.pr_number, "posted loom back-link comment on PR");
}

/// Close every open weaver issue the merged branch was working and log each
/// closure to its activity feed. The session is being torn down because its PR
/// shipped, so the tracking issues it claimed close out with it — emitting the
/// same `issue_closed` event `weaver issue close` records, so the dashboard
/// reacts identically whether a person or the merge closed them. Best-effort: a
/// hiccup here only loses the auto-close and must not prevent the session
/// archive that follows.
async fn close_claimed_issues(state: &AppState, branch: &Branch, pr: i64) {
    let closed = match weaver_core::issue::close_for_branch(
        &state.db,
        &branch.repo_root,
        &branch.branch,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(branch = %branch.branch, error = %e, "closing claimed issues on PR merge failed");
            return;
        }
    };
    for id in closed {
        events::record(
            &state.db,
            &state.bus,
            &branch.id,
            "issue_closed",
            json!({ "id": id, "reason": "pr_merged", "pr": pr }),
        )
        .await
        .ok();
        tracing::info!(branch = %branch.branch, issue = id, pr, "closed claimed issue after PR merge");
    }
}

/// Background loop: snapshot every active session's PR on a fixed cadence. A
/// no-op while `github.poll` is off or `gh` is unavailable, so it is always safe
/// to spawn. Sibling of the [`crate::monitor`] loop.
pub async fn poll(state: AppState) {
    tracing::info!(tick_s = POLL_TICK.as_secs(), "github poll loop started");
    loop {
        tokio::time::sleep(POLL_TICK).await;
        if !config::get_bool(&state.db, "github.poll", config::DEFAULT_GITHUB_POLL).await {
            continue;
        }
        if !gh_available().await {
            continue;
        }
        if let Err(e) = poll_once(&state).await {
            tracing::warn!(error = %e, "github poll tick failed");
        }
    }
}

async fn poll_once(state: &AppState) -> Result<()> {
    let archive_on_merge = config::get_bool(
        &state.db,
        "github.archive_on_merge",
        config::DEFAULT_GITHUB_ARCHIVE_ON_MERGE,
    )
    .await;
    // One active session per branch (enforced by a unique index), so iterating
    // sessions visits each candidate branch once. Engine-managed (warm) sessions
    // are infrastructure with no pull request, so the poller skips them.
    for session in session_mod::list_visible(&state.db).await? {
        if session_mod::is_terminal(&session.status) {
            continue;
        }
        let Some(branch) = branch_mod::get(&state.db, &session.branch_id).await? else {
            continue;
        };
        if let Err(e) = refresh(state, &session, &branch, archive_on_merge).await {
            tracing::debug!(branch = %branch.branch, error = %e, "github refresh failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: Option<&str>, conclusion: Option<&str>, state: Option<&str>) -> CheckJson {
        CheckJson {
            status: status.map(str::to_string),
            conclusion: conclusion.map(str::to_string),
            state: state.map(str::to_string),
        }
    }

    #[test]
    fn rollup_none_when_empty() {
        assert_eq!(rollup_checks(&[]), None);
    }

    #[test]
    fn slug_from_managed_clone_root() {
        // A managed clone lives at `<repos_dir>/owner/name`.
        let root = crate::repo::repos_dir().join("acme").join("widgets");
        assert_eq!(
            slug_from_repo_root(&root.to_string_lossy()),
            Some("acme/widgets".to_string())
        );
    }

    #[test]
    fn slug_none_for_local_path_repo() {
        // A repo forked from an arbitrary local checkout has no managed slug — the
        // back-link poster must skip it rather than invent an `owner/name`.
        assert_eq!(slug_from_repo_root("/home/someone/projects/thing"), None);
        // The repos_dir root itself (no owner/name tail) is not a slug either.
        assert_eq!(
            slug_from_repo_root(&crate::repo::repos_dir().to_string_lossy()),
            None
        );
    }

    #[test]
    fn rollup_passing_when_all_succeed() {
        let items = [
            check(Some("COMPLETED"), Some("SUCCESS"), None),
            check(Some("COMPLETED"), Some("SKIPPED"), None),
            check(None, None, Some("SUCCESS")),
        ];
        assert_eq!(rollup_checks(&items).as_deref(), Some("passing"));
    }

    #[test]
    fn rollup_pending_beats_passing_but_not_failing() {
        let pending = [
            check(Some("COMPLETED"), Some("SUCCESS"), None),
            check(Some("IN_PROGRESS"), None, None),
        ];
        assert_eq!(rollup_checks(&pending).as_deref(), Some("pending"));

        let failing = [
            check(Some("IN_PROGRESS"), None, None),
            check(Some("COMPLETED"), Some("FAILURE"), None),
        ];
        assert_eq!(rollup_checks(&failing).as_deref(), Some("failing"));
    }

    #[test]
    fn checks_went_red_fires_once_per_transition() {
        let red = snapshot_with_checks(Some("failing"));
        let green = snapshot_with_checks(Some("passing"));
        let pending = snapshot_with_checks(Some("pending"));
        let none = snapshot_with_checks(None);

        // not-failing → failing is the edge (including first-ever sighting).
        assert!(checks_went_red(None, &red));
        assert!(checks_went_red(Some("passing"), &red));
        assert!(checks_went_red(Some("pending"), &red));
        // Staying red does not re-fire.
        assert!(!checks_went_red(Some("failing"), &red));
        // A non-failing new state never fires, whatever the prior value.
        assert!(!checks_went_red(Some("failing"), &green));
        assert!(!checks_went_red(Some("failing"), &pending));
        assert!(!checks_went_red(None, &none));
    }

    fn snapshot_with_checks(checks: Option<&str>) -> GithubStatus {
        GithubStatus {
            checks: checks.map(str::to_string),
            ..snapshot("OPEN")
        }
    }

    #[test]
    fn checks_went_green_mirrors_red() {
        let green = snapshot_with_checks(Some("passing"));
        // not-passing → passing is the edge (including first-ever sighting).
        assert!(checks_went_green(None, &green));
        assert!(checks_went_green(Some("failing"), &green));
        assert!(checks_went_green(Some("pending"), &green));
        // Staying green does not re-fire; a non-passing new state never fires.
        assert!(!checks_went_green(Some("passing"), &green));
        assert!(!checks_went_green(
            None,
            &snapshot_with_checks(Some("failing"))
        ));
    }

    #[test]
    fn pr_open_and_merge_edges_fire_once() {
        let open = snapshot("OPEN");
        let merged = snapshot("MERGED");
        // Opened: unseen → OPEN, or reopened from CLOSED.
        assert!(pr_just_opened(None, &open));
        assert!(pr_just_opened(Some("CLOSED"), &open));
        assert!(!pr_just_opened(Some("OPEN"), &open));
        // Merged: any prior non-merged state → MERGED, once.
        assert!(pr_just_merged(Some("OPEN"), &merged));
        assert!(pr_just_merged(None, &merged));
        assert!(!pr_just_merged(Some("MERGED"), &merged));
    }

    #[test]
    fn review_decision_change_fires_on_new_non_null_decision() {
        let approved = GithubStatus {
            review_decision: Some("APPROVED".to_string()),
            ..snapshot("OPEN")
        };
        let changes = GithubStatus {
            review_decision: Some("CHANGES_REQUESTED".to_string()),
            ..snapshot("OPEN")
        };
        let dropped = GithubStatus {
            review_decision: None,
            ..snapshot("OPEN")
        };
        // A first decision and a decision that changes both fire.
        assert!(review_decision_changed(None, &approved));
        assert!(review_decision_changed(Some(&approved), &changes));
        // The same decision does not re-fire; dropping back to null is silent.
        assert!(!review_decision_changed(Some(&approved), &approved));
        assert!(!review_decision_changed(Some(&approved), &dropped));
    }

    #[test]
    fn parse_pr_json_normalizes_empty_strings() {
        let json = r#"{
            "number": 7, "url": "https://x/pr/7", "state": "OPEN", "title": "T",
            "isDraft": false, "reviewDecision": "", "mergeable": "MERGEABLE",
            "mergedAt": null, "statusCheckRollup": []
        }"#;
        let status = serde_json::from_str::<PrJson>(json).unwrap().into_status();
        assert_eq!(status.pr_number, 7);
        assert_eq!(status.pr_state, "OPEN");
        // Empty reviewDecision and null mergedAt collapse to None.
        assert_eq!(status.review_decision, None);
        assert_eq!(status.merged_at, None);
        // Empty rollup means "no checks", not "passing".
        assert_eq!(status.checks, None);
        assert_eq!(status.mergeable.as_deref(), Some("MERGEABLE"));
    }

    // ---- apply_snapshot: storage + archive-on-merge --------------------------

    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn snapshot(state: &str) -> GithubStatus {
        GithubStatus {
            pr_number: 5,
            pr_url: "https://example/pr/5".to_string(),
            pr_state: state.to_string(),
            pr_title: "Add the thing".to_string(),
            is_draft: false,
            review_decision: Some("APPROVED".to_string()),
            checks: Some("passing".to_string()),
            mergeable: Some("MERGEABLE".to_string()),
            merged_at: (state == "MERGED").then(now_iso),
            fetched_at: now_iso(),
        }
    }

    /// A real git repo with a `weaver/feat` worktree, an in-memory db, and a
    /// running session on that branch — the minimum to exercise `apply_snapshot`
    /// (including the worktree teardown the archive path performs).
    struct Fixture {
        _repo: tempfile::TempDir,
        state: AppState,
        session: Session,
        branch: Branch,
        work_dir: std::path::PathBuf,
    }

    async fn fixture() -> Fixture {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "Tester"]);
        git(&root, &["commit", "--allow-empty", "-m", "init"]);
        let work_dir = root.join(".worktrees/feat");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "weaver/feat",
                work_dir.to_str().unwrap(),
                "main",
            ],
        );

        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_mod::upsert(&db, &root.display().to_string(), "weaver/feat", "main")
            .await
            .unwrap();
        let session = session_mod::insert(
            &db,
            &crate::session::NewSession {
                id: "ghsess1".to_string(),
                branch_id: branch.id.clone(),
                work_dir: work_dir.display().to_string(),
                term_session: format!("weaver-ghtest-{}", std::process::id()),
                agent_kind: "shell".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: None,
                protocol: "terminal".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        let trigger = crate::github_trigger::GithubTrigger::production(db.clone());
        let state = AppState {
            db,
            bus: events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger,
            acp: crate::acp::AcpRegistry::new(),
        };
        Fixture {
            _repo: repo,
            state,
            session,
            branch,
            work_dir,
        }
    }

    #[tokio::test]
    async fn open_pr_is_stored_but_not_archived() {
        let f = fixture().await;
        apply_snapshot(&f.state, &f.session, &f.branch, &snapshot("OPEN"), true)
            .await
            .unwrap();

        // The snapshot round-trips out of the table…
        let stored = get_status(&f.state.db, &f.branch.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.pr_number, 5);
        assert_eq!(stored.pr_state, "OPEN");
        assert_eq!(stored.checks.as_deref(), Some("passing"));
        // …and an open PR leaves the live session (and its worktree) alone.
        let session = session_mod::get(&f.state.db, &f.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "running");
        assert!(f.work_dir.exists());
    }

    #[tokio::test]
    async fn explicit_mapping_and_cached_status_clear_independently() {
        let f = fixture().await;
        upsert_status(&f.state.db, &f.branch.id, &snapshot("OPEN"))
            .await
            .unwrap();
        set_mapping(&f.state.db, &f.branch.id, 42).await.unwrap();
        assert_eq!(
            get_mapping(&f.state.db, &f.branch.id).await.unwrap(),
            Some(42)
        );

        clear_status(&f.state.db, &f.branch.id).await.unwrap();
        assert!(get_status(&f.state.db, &f.branch.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            get_mapping(&f.state.db, &f.branch.id).await.unwrap(),
            Some(42)
        );

        clear_mapping(&f.state.db, &f.branch.id).await.unwrap();
        assert_eq!(get_mapping(&f.state.db, &f.branch.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn automatic_discovery_follows_the_worktree_head_branch() {
        let f = fixture().await;
        git(&f.work_dir, &["switch", "-c", "agent/20260721-fix-7473"]);

        assert_eq!(
            discovery_branch(&f.session, &f.branch).await,
            "agent/20260721-fix-7473"
        );
        assert_eq!(f.branch.branch, "weaver/feat", "loom identity stays stable");
    }

    #[tokio::test]
    async fn merged_pr_archives_the_session_and_removes_the_worktree() {
        let f = fixture().await;
        apply_snapshot(&f.state, &f.session, &f.branch, &snapshot("MERGED"), true)
            .await
            .unwrap();

        let session = session_mod::get(&f.state.db, &f.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "archived");
        // The worktree is torn down — that's the "archive the worktree" promise.
        assert!(!f.work_dir.exists());
        // The merged snapshot is still queryable for the archived session.
        let stored = get_status(&f.state.db, &f.branch.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.pr_state, "MERGED");
    }

    #[tokio::test]
    async fn merged_pr_does_not_archive_a_recovered_session() {
        let f = fixture().await;
        tags::set(
            &f.state.db,
            &f.branch.id,
            tags::RECOVERED_KEY,
            tags::RECOVERED_VALUE,
            "session recovered",
            "loom",
        )
        .await
        .unwrap();
        let issue = weaver_core::issue::add(
            &f.state.db,
            &weaver_core::issue::NewIssue {
                repo_root: f.branch.repo_root.clone(),
                claimed_branch: Some(f.branch.branch.clone()),
                title: "keep working".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        apply_snapshot(&f.state, &f.session, &f.branch, &snapshot("MERGED"), true)
            .await
            .unwrap();

        let session = session_mod::get(&f.state.db, &f.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "running");
        assert!(f.work_dir.exists());
        let open_issue = weaver_core::issue::get(&f.state.db, issue.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open_issue.status, "open");
    }

    #[tokio::test]
    async fn malformed_recovered_tag_does_not_suppress_archive_on_merge() {
        let f = fixture().await;
        tags::set(
            &f.state.db,
            &f.branch.id,
            tags::RECOVERED_KEY,
            "false",
            "manual",
            "manual",
        )
        .await
        .unwrap();

        apply_snapshot(&f.state, &f.session, &f.branch, &snapshot("MERGED"), true)
            .await
            .unwrap();

        let session = session_mod::get(&f.state.db, &f.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "archived");
        assert!(!f.work_dir.exists());
    }

    #[tokio::test]
    async fn merged_pr_closes_the_branchs_claimed_issues() {
        let f = fixture().await;
        let issue = weaver_core::issue::add(
            &f.state.db,
            &weaver_core::issue::NewIssue {
                repo_root: f.branch.repo_root.clone(),
                claimed_branch: Some(f.branch.branch.clone()),
                title: "ship the feature".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        apply_snapshot(&f.state, &f.session, &f.branch, &snapshot("MERGED"), true)
            .await
            .unwrap();

        // The tracking issue closes out with the merged session…
        let closed = weaver_core::issue::get(&f.state.db, issue.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(closed.status, "closed");
        assert_eq!(
            closed.claimed_branch, None,
            "the archived session no longer owns the closed issue"
        );
        // …and the closure lands on the activity feed as an `issue_closed` event,
        // just as `weaver issue close` would have recorded it.
        let logged = events::history(&f.state.db, &f.branch.id, 50)
            .await
            .unwrap()
            .into_iter()
            .any(|e| e.kind == "issue_closed" && e.data["id"] == issue.id);
        assert!(logged, "an issue_closed event was recorded for the merge");
    }

    #[tokio::test]
    async fn merged_pr_is_left_alone_when_archive_on_merge_is_off() {
        let f = fixture().await;
        apply_snapshot(&f.state, &f.session, &f.branch, &snapshot("MERGED"), false)
            .await
            .unwrap();

        let session = session_mod::get(&f.state.db, &f.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "running");
        assert!(f.work_dir.exists());
    }

    // -- the status card ----------------------------------------------------

    #[test]
    fn parse_wiring_accepts_a_thread_and_rejects_noise() {
        assert_eq!(
            parse_wiring("acme/widgets#87"),
            Some(("acme/widgets".to_string(), 87))
        );
        assert_eq!(
            parse_wiring("  acme/widgets#87  "),
            Some(("acme/widgets".to_string(), 87))
        );
        for bad in [
            "",
            "acme/widgets",
            "#87",
            "acme#87",
            "acme/widgets#0",
            "acme/widgets#-3",
            "acme/widgets#seven",
            "a/b/c#7",
            // The slug feeds GitHub API paths: the repo charset gate applies,
            // and only the bare documented form is a wiring (no URL forms).
            "../evil#7",
            "acme/..#7",
            "acme/wid gets#7",
            "acme/wid%2Fgets#7",
            "https://github.com/acme/widgets#7",
        ] {
            assert_eq!(parse_wiring(bad), None, "{bad:?} must not parse");
        }
    }

    fn status_event(value: &str, note: &str, by: &str) -> weaver_core::events::Event {
        weaver_core::events::Event {
            id: 0,
            branch_id: "b".to_string(),
            kind: "tag".to_string(),
            data: json!({ "key": "attention", "value": value, "note": note, "by": by }),
            created_at: "2026-07-18T21:04:05.000Z".to_string(),
        }
    }

    #[test]
    fn status_card_renders_the_trail_oldest_first() {
        let events = vec![
            status_event("", "mapping the code", "agent"),
            // A manual UI edit must never read as agent progress.
            status_event("attention", "operator note", "manual"),
            // An unrelated tag event is not part of the trail.
            weaver_core::events::Event {
                id: 0,
                branch_id: "b".to_string(),
                kind: "tag".to_string(),
                data: json!({ "key": "idle", "value": "idle", "note": "", "by": "loom" }),
                created_at: "2026-07-18T21:05:00.000Z".to_string(),
            },
            // A bare `weaver status ok` (no message) says nothing new.
            status_event("", "", "agent"),
            status_event("blocked", "build broken", "agent"),
        ];
        let card = render_status_card("http://loom/s/abc", &[], &events);
        let lines: Vec<&str> = card.lines().collect();
        assert_eq!(lines[0], "On it — http://loom/s/abc");
        assert_eq!(lines[1], "");
        assert!(lines[2].contains("mapping the code") && lines[2].contains("\u{1f7e2}"));
        assert!(lines[3].contains("**blocked** — build broken") && lines[3].contains("\u{1f534}"));
        assert_eq!(lines.len(), 4, "manual/idle/silent events render nothing");
    }

    #[test]
    fn status_card_with_no_trail_is_just_the_header() {
        assert_eq!(
            render_status_card("http://loom/s/abc", &[], &[]),
            "On it — http://loom/s/abc"
        );
    }

    #[test]
    fn status_card_links_published_artifacts() {
        let card = render_status_card(
            "http://loom/s/abc",
            &["design".to_string(), "plan".to_string()],
            &[status_event("", "drafted the design", "agent")],
        );
        let lines: Vec<&str> = card.lines().collect();
        assert_eq!(
            lines[1],
            "Docs: [design](http://loom/s/abc/artifacts/design) · [plan](http://loom/s/abc/artifacts/plan)"
        );
        assert!(lines[3].contains("drafted the design"));
    }

    #[test]
    fn status_card_escapes_hostile_artifact_names() {
        // Names are agent-chosen free text; brackets must not break the link
        // Markdown and the URL segment must be percent-encoded.
        let card = render_status_card("http://loom/s/abc", &["a](x) [b".to_string()], &[]);
        assert_eq!(
            card.lines().nth(1).unwrap(),
            "Docs: [a\\](x) \\[b](http://loom/s/abc/artifacts/a%5D%28x%29%20%5Bb)"
        );
    }

    #[test]
    fn status_card_collapses_a_long_trail() {
        let events: Vec<_> = (0..20)
            .map(|i| status_event("", &format!("step {i}"), "agent"))
            .collect();
        let card = render_status_card("http://loom/s/abc", &[], &events);
        assert!(card.contains("…5 earlier updates"));
        assert!(!card.contains("step 4"), "collapsed entries are dropped");
        assert!(card.contains("step 5") && card.contains("step 19"));
    }
}
