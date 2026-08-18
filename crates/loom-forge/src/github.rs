//! GitHub integration through Loom's configured GitHub App.
//!
//! It owns PR status polling ([`poll`], [`refresh`]): the background loop
//! snapshots each live session's pull request (link, review decision, check
//! rollup) into the `branch_github` table, and archives a session — closing the
//! weaver issues it was working — once its PR merges. The snapshot rides along
//! on `BranchView`; the dashboard renders it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::db::{now_iso, Db};
use crate::session::{self as session_mod, Session};
use crate::AppState;
use crate::{branch as branch_mod, config, events};
use weaver_core::branch::Branch;
use weaver_core::github::GithubStatus;
use weaver_core::tags;
use weaver_core::BoxFut;

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: String,
}

// ---------------------------------------------------------------------------
// PR status snapshots
//
// The latest pull-request snapshot loom found for a branch, persisted in the
// `branch_github` table (one row per branch) and served inside `BranchView`.
// The background `poll` loop keeps it fresh; `refresh` does one branch on
// demand. Everything degrades to "no snapshot" when the App is unavailable or
// the repo has no GitHub remote.
// ---------------------------------------------------------------------------

const POLL_TICK: Duration = Duration::from_secs(60);
const ACTIVE_POLL_WINDOW_SECS: i64 = 10 * 60;
const RECENT_POLL_WINDOW_SECS: i64 = 24 * 60 * 60;
const RECENT_POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);
const STALE_POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

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
/// `weaver status set --tag ok`) render `None`.
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

/// Render the status card: the "On it" header linking the session, any
/// published-document links, then the trail of `weaver status` reports (oldest first, capped at
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
        body.push_str(&format!("\n{}", links.join(" · ")));
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
fn sync_status_comment_once<'a>(state: &'a AppState, branch_id: &'a str) -> BoxFut<'a, bool> {
    Box::pin(sync_status_comment_once_inner(state, branch_id))
}

async fn sync_status_comment_once_inner(state: &AppState, branch_id: &str) -> bool {
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
        &crate::links::session_url(&base, &session.id),
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

/// The GraphQL pull-request shape. Internal — callers see [`GithubStatus`].
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
    #[serde(rename = "headRefOid", default)]
    head_ref_oid: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Option<Vec<CheckJson>>,
}

/// GraphQL returns the check rollup as a connection, while `gh pr view --json`
/// flattens that connection to an array. Keep the public/manual refresh path on
/// the CLI's stable JSON shape and adapt the batched background response here.
#[derive(Debug, Deserialize)]
struct GraphqlCheckRollup {
    contexts: GraphqlCheckConnection,
}

#[derive(Debug, Deserialize)]
struct GraphqlCheckConnection {
    nodes: Vec<CheckJson>,
}

#[derive(Debug, Deserialize)]
struct GraphqlPrJson {
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
    #[serde(rename = "headRefOid", default)]
    head_ref_oid: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Option<GraphqlCheckRollup>,
}

impl GraphqlPrJson {
    fn into_status(self) -> GithubStatus {
        let checks = self
            .status_check_rollup
            .map(|rollup| rollup.contexts.nodes)
            .unwrap_or_default();
        PrJson {
            number: self.number,
            url: self.url,
            state: self.state,
            title: self.title,
            is_draft: self.is_draft,
            review_decision: self.review_decision,
            mergeable: self.mergeable,
            merged_at: self.merged_at,
            head_ref_oid: self.head_ref_oid,
            updated_at: self.updated_at,
            status_check_rollup: Some(checks),
        }
        .into_status()
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlPrConnection {
    nodes: Vec<GraphqlPrJson>,
}

#[derive(Debug, Deserialize)]
struct GraphqlEnvelope {
    #[serde(default)]
    data: Option<GraphqlData>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
    repository: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Default)]
struct BatchSnapshots {
    by_head: HashMap<String, GithubStatus>,
    by_number: HashMap<i64, GithubStatus>,
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
            head_sha: nonempty(self.head_ref_oid),
            // GitHub's PR `updatedAt` is only the initial estimate. Once a
            // snapshot exists, `apply_snapshot` holds this timestamp steady
            // until `headRefOid` changes, excluding comments/labels/reviews.
            head_updated_at: nonempty(self.updated_at),
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

async fn app_and_slug<'a>(
    state: &'a AppState,
    repo_root: &Path,
) -> Result<(&'a crate::github_app::GithubApp, String)> {
    let slug = crate::repo::github_slug_for_root(&state.db, repo_root)
        .await?
        .context("repository has no registered GitHub identity")?;
    let app = state.trigger.app().context("GitHub App is unavailable")?;
    Ok((app, slug))
}

pub async fn fetch_pr(
    state: &AppState,
    repo_root: &Path,
    number: i64,
) -> Result<Option<GithubStatus>> {
    let (app, slug) = app_and_slug(state, repo_root).await?;
    let snapshots = fetch_pr_batch(app, &slug, &[], &[number]).await?;
    Ok(snapshots.by_number.get(&number).cloned())
}

pub async fn fetch_open_pr(
    state: &AppState,
    repo_root: &Path,
    branch: &str,
) -> Result<Option<GithubStatus>> {
    let (app, slug) = app_and_slug(state, repo_root).await?;
    let snapshots = fetch_pr_batch(app, &slug, &[branch.to_string()], &[]).await?;
    Ok(snapshots.by_head.get(branch).cloned())
}

/// Fetch every due branch's current open PR plus every previously-known PR in
/// one GraphQL request. Aliases multiplex the independent branch/number
/// lookups without downloading a repository's unrelated PRs. Exact-number
/// lookups keep terminal transitions visible after GitHub removes a merged or
/// closed PR from the open-head result.
async fn fetch_pr_batch(
    app: &crate::github_app::GithubApp,
    slug: &str,
    heads: &[String],
    known_numbers: &[i64],
) -> Result<BatchSnapshots> {
    let query = build_batch_query(slug, heads, known_numbers)?;
    let repo = crate::repo::parse_slug(slug).map_err(anyhow::Error::msg)?;
    let response = app.graphql(&repo, &query).await?;
    parse_batch_response(&response, slug, heads, known_numbers)
}

fn build_batch_query(slug: &str, heads: &[String], known_numbers: &[i64]) -> Result<String> {
    let parsed = crate::repo::parse_slug(slug).map_err(anyhow::Error::msg)?;
    let mut selections = String::new();
    for (index, head) in heads.iter().enumerate() {
        let head = serde_json::to_string(head).context("encoding branch name for GraphQL")?;
        selections.push_str(&format!(
            "branch{index}: pullRequests(first: 1, states: [OPEN], headRefName: {head}) {{ nodes {{ ...PrFields }} }}\n"
        ));
    }
    for (index, number) in known_numbers.iter().enumerate() {
        selections.push_str(&format!(
            "known{index}: pullRequest(number: {number}) {{ ...PrFields }}\n"
        ));
    }
    let owner = serde_json::to_string(&parsed.owner).context("encoding repository owner")?;
    let name = serde_json::to_string(&parsed.name).context("encoding repository name")?;
    Ok(format!(
        r#"query {{
  repository(owner: {owner}, name: {name}) {{
    {selections}
  }}
}}
fragment PrFields on PullRequest {{
  number url state title isDraft reviewDecision mergeable mergedAt headRefOid updatedAt
  statusCheckRollup {{
    contexts(first: 100) {{
      nodes {{
        __typename
        ... on CheckRun {{ status conclusion }}
        ... on StatusContext {{ state }}
      }}
    }}
  }}
}}"#
    ))
}

fn parse_batch_response(
    bytes: &[u8],
    slug: &str,
    heads: &[String],
    known_numbers: &[i64],
) -> Result<BatchSnapshots> {
    let envelope: GraphqlEnvelope =
        serde_json::from_slice(bytes).context("parsing batched GitHub GraphQL response")?;
    if !envelope.errors.is_empty() {
        bail!(
            "GitHub GraphQL query failed for {slug}: {}",
            serde_json::to_string(&envelope.errors).unwrap_or_default()
        );
    }
    let repository = envelope
        .data
        .and_then(|data| data.repository)
        .ok_or_else(|| anyhow::anyhow!("GitHub repository {slug} was not found"))?;

    let mut snapshots = BatchSnapshots::default();
    for (index, head) in heads.iter().enumerate() {
        let Some(value) = repository.get(&format!("branch{index}")) else {
            continue;
        };
        let connection: GraphqlPrConnection =
            serde_json::from_value(value.clone()).context("parsing batched open-PR result")?;
        if let Some(pr) = connection.nodes.into_iter().next() {
            snapshots.by_head.insert(head.clone(), pr.into_status());
        }
    }
    for (index, number) in known_numbers.iter().enumerate() {
        let Some(value) = repository.get(&format!("known{index}")) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let pr: GraphqlPrJson =
            serde_json::from_value(value.clone()).context("parsing batched known-PR result")?;
        snapshots.by_number.insert(*number, pr.into_status());
    }
    Ok(snapshots)
}

/// The stored snapshot for a branch, if one has been fetched.
pub async fn get_status(db: &Db, branch_id: &str) -> Result<Option<GithubStatus>> {
    let row = sqlx::query_as::<_, GithubStatus>(
        "SELECT pr_number, pr_url, pr_state, pr_title, is_draft, review_decision,
                checks, mergeable, merged_at, head_sha, head_updated_at, fetched_at
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
            review_decision, checks, mergeable, merged_at, head_sha,
            head_updated_at, fetched_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(branch_id) DO UPDATE SET
           pr_number = excluded.pr_number, pr_url = excluded.pr_url,
           pr_state = excluded.pr_state, pr_title = excluded.pr_title,
           is_draft = excluded.is_draft, review_decision = excluded.review_decision,
           checks = excluded.checks, mergeable = excluded.mergeable,
           merged_at = excluded.merged_at, head_sha = excluded.head_sha,
           head_updated_at = excluded.head_updated_at,
           fetched_at = excluded.fetched_at",
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
    .bind(&s.head_sha)
    .bind(&s.head_updated_at)
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
    let repo_root = PathBuf::from(&branch.repo_root);
    let previous = get_status(&state.db, &branch.id).await?;
    let mapping = get_mapping(&state.db, &branch.id).await?;
    let fetched = if let Some(number) = mapping {
        fetch_pr(state, &repo_root, number).await?
    } else {
        // Automatic mode always asks GitHub which open PR is current instead of
        // treating yesterday's cached number as authoritative. Only when the
        // branch has no open PR do we revisit the previous number once, to
        // preserve its close/merge transition before the snapshot is cleared.
        let head = discovery_branch(session, branch).await;
        let current = fetch_open_pr(state, &repo_root, &head).await?;
        if current.is_some() {
            current
        } else if previous.as_ref().is_some_and(|pr| pr.pr_state == "OPEN") {
            let number = previous.as_ref().expect("checked above").pr_number;
            fetch_pr(state, &repo_root, number).await?
        } else {
            None
        }
    };
    apply_refresh_result(state, session, branch, fetched, archive_on_merge).await
}

/// Apply the common result of either a one-session CLI lookup or a batched
/// GraphQL lookup. Automatic discovery must clear a stale snapshot when no PR
/// remains, while a present snapshot follows the same event/archive path.
async fn apply_refresh_result(
    state: &AppState,
    session: &Session,
    branch: &Branch,
    fetched: Option<GithubStatus>,
    archive_on_merge: bool,
) -> Result<Option<GithubStatus>> {
    let previous = get_status(&state.db, &branch.id).await?;
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
pub async fn apply_snapshot(
    state: &AppState,
    session: &Session,
    branch: &Branch,
    snap: &GithubStatus,
    archive_on_merge: bool,
) -> Result<()> {
    let prev = get_status(&state.db, &branch.id).await?;
    let mut current = snap.clone();
    if let Some(previous) = prev.as_ref().filter(|previous| {
        previous.pr_number == current.pr_number
            && previous.head_sha.is_some()
            && previous.head_sha == current.head_sha
    }) {
        current.head_updated_at = previous.head_updated_at.clone().or(current.head_updated_at);
    }
    upsert_status(&state.db, &branch.id, &current).await?;
    let changed = prev
        .as_ref()
        .is_none_or(|previous| current.meaningfully_differs_from(previous));
    if changed {
        events::record(
            &state.db,
            &state.bus,
            &branch.id,
            "github",
            current.event_data(),
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
            pr_just_opened(prev_state, &current),
            "pr_opened",
            json!({ "pr": snap.pr_number }),
        ),
        (
            pr_just_merged(prev_state, &current),
            "pr_merged",
            json!({ "pr": snap.pr_number }),
        ),
        (
            checks_went_red(prev_checks, &current),
            "pr_red",
            json!({ "pr": snap.pr_number, "checks": "failing" }),
        ),
        (
            checks_went_green(prev_checks, &current),
            "pr_green",
            json!({ "pr": snap.pr_number, "checks": "passing" }),
        ),
        (
            review_decision_changed(prev.as_ref(), &current),
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
    // logging a failure — on every poll. Skipped once the branch is tagged
    // linked (here or by the `@loom` reply).
    if pr_just_opened(prev_state, &current) {
        maybe_post_backlink(state, session, branch, &current).await;
    }

    let recovered = tags::get(&state.db, &branch.id, tags::RECOVERED_KEY)
        .await?
        .is_some_and(|tag| tag.value == tags::RECOVERED_VALUE);
    if archive_on_merge
        && current.pr_state == "MERGED"
        && !session_mod::is_terminal(&session.status)
        && !recovered
    {
        // Snapshot the shipped work before archive releases its claims. It is
        // closed only after auto_archive has rechecked the per-session opt-out
        // and successfully torn the session down.
        let claimed = claimed_issues(state, branch).await;
        // The merge is already on the record as a `github` event (above) and the
        // archive records a `status` event, so no extra log line is needed.
        match crate::lifecycle::auto_archive(state, session, branch).await {
            Ok(Some(_)) => {
                close_claimed_issues(state, branch, current.pr_number, &claimed).await;
                tracing::info!(
                    branch = %branch.branch,
                    pr = current.pr_number,
                    "archived session after PR merge"
                );
            }
            Ok(None) => tracing::info!(
                branch = %branch.branch,
                pr = snap.pr_number,
                "archive-on-merge skipped by session tag"
            ),
            Err(e) => tracing::warn!(
                branch = %branch.branch,
                error = %e,
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
        crate::links::session_url(&base, &session.id)
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

/// Snapshot the open issues a branch owns before archive releases its claims.
/// Best-effort: issue cleanup must never prevent the session teardown.
async fn claimed_issues(state: &AppState, branch: &Branch) -> Vec<weaver_core::issue::Issue> {
    match weaver_core::issue::list_for_branch(&state.db, &branch.repo_root, &branch.branch, false)
        .await
    {
        Ok(issues) => issues,
        Err(e) => {
            tracing::warn!(branch = %branch.branch, error = %e, "could not inspect claimed issues before merge archive");
            Vec::new()
        }
    }
}

/// Close the issues captured before a successful merge archive and log each
/// closure to its activity feed. Re-read each row first: archive unclaimed it,
/// but another session may already have picked it up, in which case it is no
/// longer ours to close.
async fn close_claimed_issues(
    state: &AppState,
    branch: &Branch,
    pr: i64,
    claimed: &[weaver_core::issue::Issue],
) {
    for captured in claimed {
        let Ok(Some(current)) = weaver_core::issue::get(&state.db, captured.id).await else {
            continue;
        };
        if current.status != "open" || current.claimed_branch.is_some() {
            continue;
        }
        if let Err(e) = weaver_core::issue::close(&state.db, current.id).await {
            tracing::warn!(branch = %branch.branch, issue = current.id, error = %e, "could not close claimed issue after merge");
            continue;
        }
        events::record(
            &state.db,
            &state.bus,
            &branch.id,
            "issue_closed",
            json!({ "id": current.id, "reason": "pr_merged", "pr": pr }),
        )
        .await
        .ok();
        tracing::info!(branch = %branch.branch, issue = current.id, pr, "closed claimed issue after PR merge");
    }
}

/// Background loop: snapshot live sessions' PRs on an activity-aware cadence. A
/// no-op while `github.poll` is off or the GitHub App is unavailable, so it is
/// always safe to spawn. Sibling of the [`crate::monitor`] loop.
pub async fn poll(state: AppState) {
    tracing::info!(tick_s = POLL_TICK.as_secs(), "github poll loop started");
    let mut ticks = tokio::time::interval(POLL_TICK);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Snapshots persist successful PR fetches, but an empty discovery result has
    // no row to carry its fetch time. Keep attempts in memory so every quiet
    // branch follows the same tiered cadence instead of retrying failures or
    // empty results every minute. Restarting loom intentionally makes every live
    // branch immediately eligible once.
    let mut last_polled = HashMap::new();
    loop {
        ticks.tick().await;
        if !config::get_bool(&state.db, "github.poll", config::DEFAULT_GITHUB_POLL).await {
            continue;
        }
        let Some(app) = state.trigger.app() else {
            continue;
        };
        if !app.is_configured().await {
            continue;
        }
        if let Err(e) = poll_once(&state, &mut last_polled).await {
            tracing::warn!(error = %e, "github poll tick failed");
        }
    }
}

#[derive(Debug)]
struct PollCandidate {
    session: Session,
    branch: Branch,
    head: String,
    mapping: Option<i64>,
    previous: Option<GithubStatus>,
}

/// Select a GitHub cadence from session activity: the existing one-minute path
/// for active work, ten minutes for the rest of the first quiet day, and hourly
/// thereafter. A never-touched session uses its creation time, matching the
/// monitor's activity semantics. Corrupt timestamps stay on the fastest tier so
/// bad local data cannot silently delay lifecycle handling forever.
fn github_poll_interval(
    last_activity_at: Option<&str>,
    created_at: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Duration {
    let timestamp = last_activity_at.unwrap_or(created_at);
    chrono::DateTime::parse_from_rfc3339(timestamp).map_or(POLL_TICK, |activity| {
        let age = now.signed_duration_since(activity.with_timezone(&chrono::Utc));
        if age <= chrono::Duration::seconds(ACTIVE_POLL_WINDOW_SECS) {
            POLL_TICK
        } else if age < chrono::Duration::seconds(RECENT_POLL_WINDOW_SECS) {
            RECENT_POLL_INTERVAL
        } else {
            STALE_POLL_INTERVAL
        }
    })
}

fn due_for_github_poll(
    session: &Session,
    last_polled: Option<&chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let interval = github_poll_interval(
        session.last_activity_at.as_deref(),
        &session.created_at,
        now,
    );
    poll_interval_has_elapsed(last_polled, interval, now)
}

fn poll_interval_has_elapsed(
    last_polled: Option<&chrono::DateTime<chrono::Utc>>,
    interval: Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    last_polled.is_none_or(|last| {
        now.signed_duration_since(*last)
            .to_std()
            .map_or(true, |elapsed| elapsed >= interval)
    })
}

fn snapshot_for_candidate(
    mapping: Option<i64>,
    previous: Option<&GithubStatus>,
    head: &str,
    snapshots: &BatchSnapshots,
) -> Option<GithubStatus> {
    if let Some(number) = mapping {
        return snapshots.by_number.get(&number).cloned();
    }
    if let Some(current) = snapshots.by_head.get(head) {
        return Some(current.clone());
    }
    previous
        .filter(|pr| pr.pr_state == "OPEN")
        .and_then(|pr| snapshots.by_number.get(&pr.pr_number))
        .cloned()
}

async fn poll_once(
    state: &AppState,
    last_polled: &mut HashMap<String, chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    let archive_on_merge = config::get_bool(
        &state.db,
        "github.archive_on_merge",
        config::DEFAULT_GITHUB_ARCHIVE_ON_MERGE,
    )
    .await;
    // One active session per branch (enforced by a unique index), so iterating
    // sessions visits each candidate branch once. Grouping by repository lets
    // one GraphQL request multiplex every branch lookup and every exact known-PR
    // lookup. Engine-managed (warm) sessions are absent from `list_visible`.
    let mut repositories: BTreeMap<String, Vec<PollCandidate>> = BTreeMap::new();
    let now = chrono::Utc::now();
    for session in session_mod::list_visible(&state.db).await? {
        if session_mod::is_terminal(&session.status)
            || !due_for_github_poll(&session, last_polled.get(&session.branch_id), now)
        {
            continue;
        }
        let Some(branch) = branch_mod::get(&state.db, &session.branch_id).await? else {
            continue;
        };
        let candidate = PollCandidate {
            head: discovery_branch(&session, &branch).await,
            mapping: get_mapping(&state.db, &branch.id).await?,
            previous: get_status(&state.db, &branch.id).await?,
            session,
            branch,
        };
        repositories
            .entry(candidate.branch.repo_root.clone())
            .or_default()
            .push(candidate);
    }

    for (repo_root, candidates) in repositories {
        let path = PathBuf::from(&repo_root);
        let Some(slug) = crate::repo::github_slug_for_root(&state.db, &path).await? else {
            tracing::debug!(repo = %repo_root, "github batch skipped: repository has no GitHub remote");
            for candidate in candidates {
                last_polled.insert(candidate.branch.id, now);
            }
            continue;
        };
        let Some(app) = state.trigger.app() else {
            tracing::debug!(repo = %repo_root, "github batch skipped: GitHub App unavailable");
            continue;
        };
        let heads: Vec<String> = candidates
            .iter()
            .map(|candidate| candidate.head.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let known_numbers: Vec<i64> = candidates
            .iter()
            .flat_map(|candidate| {
                candidate.mapping.into_iter().chain(
                    candidate
                        .previous
                        .as_ref()
                        .filter(|pr| pr.pr_state == "OPEN")
                        .map(|pr| pr.pr_number),
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let fetched = fetch_pr_batch(app, &slug, &heads, &known_numbers).await;
        for candidate in &candidates {
            last_polled.insert(candidate.branch.id.clone(), now);
        }
        let snapshots = match fetched {
            Ok(snapshots) => snapshots,
            Err(e) => {
                tracing::debug!(repo = %slug, error = %e, "github batch refresh failed");
                continue;
            }
        };

        for candidate in candidates {
            let fetched = snapshot_for_candidate(
                candidate.mapping,
                candidate.previous.as_ref(),
                &candidate.head,
                &snapshots,
            );
            if let Err(e) = apply_refresh_result(
                state,
                &candidate.session,
                &candidate.branch,
                fetched,
                archive_on_merge,
            )
            .await
            {
                tracing::debug!(branch = %candidate.branch.branch, error = %e, "github refresh failed");
            }
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
            "mergedAt": null, "headRefOid": "abc123", "updatedAt": "2026-08-03T21:42:17Z",
            "statusCheckRollup": []
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
        assert_eq!(status.head_sha.as_deref(), Some("abc123"));
        assert_eq!(
            status.head_updated_at.as_deref(),
            Some("2026-08-03T21:42:17Z")
        );
    }

    #[test]
    fn batch_query_multiplexes_heads_and_known_numbers() {
        let query = build_batch_query(
            "acme/widgets",
            &["weaver/one".to_string(), "quoted\"branch".to_string()],
            &[7, 42],
        )
        .unwrap();
        assert!(query.contains(
            "branch0: pullRequests(first: 1, states: [OPEN], headRefName: \"weaver/one\")"
        ));
        assert!(query.contains("headRefName: \"quoted\\\"branch\""));
        assert!(query.contains("known0: pullRequest(number: 7)"));
        assert!(query.contains("known1: pullRequest(number: 42)"));
        assert_eq!(query.matches("repository(owner:").count(), 1);
    }

    #[test]
    fn batch_response_indexes_open_heads_and_terminal_known_prs() {
        let pr = |number: i64, state: &str, checks: &str| {
            json!({
                "number": number,
                "url": format!("https://example/pr/{number}"),
                "state": state,
                "title": format!("PR {number}"),
                "isDraft": false,
                "reviewDecision": "APPROVED",
                "mergeable": "MERGEABLE",
                "mergedAt": (state == "MERGED").then_some("2026-08-16T00:00:00Z"),
                "headRefOid": format!("head-{number}"),
                "updatedAt": "2026-08-16T00:00:00Z",
                "statusCheckRollup": {
                    "contexts": { "nodes": [{
                        "__typename": "CheckRun",
                        "status": "COMPLETED",
                        "conclusion": checks,
                    }] }
                }
            })
        };
        let body = json!({
            "data": { "repository": {
                "branch0": { "nodes": [pr(7, "OPEN", "SUCCESS")] },
                "branch1": { "nodes": [] },
                "known0": pr(7, "OPEN", "SUCCESS"),
                "known1": pr(42, "MERGED", "FAILURE"),
            }}
        });
        let heads = ["weaver/one".to_string(), "weaver/two".to_string()];
        let snapshots = parse_batch_response(
            &serde_json::to_vec(&body).unwrap(),
            "acme/widgets",
            &heads,
            &[7, 42],
        )
        .unwrap();

        assert_eq!(snapshots.by_head["weaver/one"].pr_number, 7);
        assert!(!snapshots.by_head.contains_key("weaver/two"));
        assert_eq!(snapshots.by_number[&7].checks.as_deref(), Some("passing"));
        assert_eq!(snapshots.by_number[&42].pr_state, "MERGED");
        assert_eq!(snapshots.by_number[&42].checks.as_deref(), Some("failing"));
    }

    #[test]
    fn github_poll_cadence_slows_with_session_activity() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for (last_activity, created_at, expected) in [
            (
                Some("2026-08-16T11:50:00Z"),
                "2026-08-01T00:00:00Z",
                POLL_TICK,
            ),
            (
                Some("2026-08-16T11:49:59Z"),
                "2026-08-16T11:59:00Z",
                RECENT_POLL_INTERVAL,
            ),
            (None, "2026-08-15T12:00:01Z", RECENT_POLL_INTERVAL),
            (None, "2026-08-15T12:00:00Z", STALE_POLL_INTERVAL),
            (Some("corrupt"), "2026-08-16T11:00:00Z", POLL_TICK),
        ] {
            assert_eq!(
                github_poll_interval(last_activity, created_at, now),
                expected
            );
        }
    }

    #[test]
    fn github_poll_waits_for_the_selected_interval() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let nine_minutes_ago = now - chrono::Duration::minutes(9);
        let ten_minutes_ago = now - chrono::Duration::minutes(10);

        assert!(poll_interval_has_elapsed(None, RECENT_POLL_INTERVAL, now));
        assert!(!poll_interval_has_elapsed(
            Some(&nine_minutes_ago),
            RECENT_POLL_INTERVAL,
            now,
        ));
        assert!(poll_interval_has_elapsed(
            Some(&ten_minutes_ago),
            RECENT_POLL_INTERVAL,
            now,
        ));
    }

    #[test]
    fn batch_selection_keeps_known_pr_terminal_transitions() {
        let previous = snapshot("OPEN");
        let mut merged = snapshot("MERGED");
        merged.pr_number = previous.pr_number;
        let mut snapshots = BatchSnapshots::default();
        snapshots
            .by_number
            .insert(previous.pr_number, merged.clone());

        let selected =
            snapshot_for_candidate(None, Some(&previous), "weaver/finished", &snapshots).unwrap();
        assert_eq!(selected.pr_state, "MERGED");

        let mut replacement = snapshot("OPEN");
        replacement.pr_number = 99;
        snapshots
            .by_head
            .insert("weaver/finished".to_string(), replacement.clone());
        let selected =
            snapshot_for_candidate(None, Some(&previous), "weaver/finished", &snapshots).unwrap();
        assert_eq!(selected.pr_number, 99, "a current open head wins");
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
            head_sha: Some("head-one".to_string()),
            head_updated_at: Some("2026-08-03T21:42:17Z".to_string()),
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
            ctx: crate::Ctx {
                db,
                bus: events::EventBus::new(),
                addr: "127.0.0.1:0".to_string(),
            },
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger,
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
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
        assert_eq!(stored.head_sha.as_deref(), Some("head-one"));
        assert_eq!(
            stored.head_updated_at.as_deref(),
            Some("2026-08-03T21:42:17Z")
        );
        // …and an open PR leaves the live session (and its worktree) alone.
        let session = session_mod::get(&f.state.db, &f.session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "running");
        assert!(f.work_dir.exists());
    }

    #[tokio::test]
    async fn head_freshness_changes_only_with_the_head_sha() {
        let f = fixture().await;
        let first = snapshot("OPEN");
        apply_snapshot(&f.state, &f.session, &f.branch, &first, false)
            .await
            .unwrap();

        let mut metadata_only = first.clone();
        metadata_only.head_updated_at = Some("2026-08-04T10:00:00Z".to_string());
        apply_snapshot(&f.state, &f.session, &f.branch, &metadata_only, false)
            .await
            .unwrap();
        let stored = get_status(&f.state.db, &f.branch.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.head_updated_at.as_deref(),
            Some("2026-08-03T21:42:17Z"),
            "unrelated PR metadata must not make the head look fresh"
        );

        let mut pushed = metadata_only;
        pushed.head_sha = Some("head-two".to_string());
        apply_snapshot(&f.state, &f.session, &f.branch, &pushed, false)
            .await
            .unwrap();
        let stored = get_status(&f.state.db, &f.branch.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.head_sha.as_deref(), Some("head-two"));
        assert_eq!(
            stored.head_updated_at.as_deref(),
            Some("2026-08-04T10:00:00Z")
        );
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
    async fn merged_pr_respects_the_session_auto_archive_opt_out() {
        let f = fixture().await;
        tags::set(
            &f.state.db,
            &f.branch.id,
            tags::AUTO_ARCHIVE_KEY,
            tags::AUTO_ARCHIVE_DISABLED_VALUE,
            "keep this session",
            "manual",
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
        assert_eq!(
            open_issue.claimed_branch.as_deref(),
            Some(f.branch.branch.as_str())
        );
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
            // A status update without a message says nothing new.
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
            "[design](http://loom/s/abc/artifacts/design) · [plan](http://loom/s/abc/artifacts/plan)"
        );
        assert!(!card.contains("Docs:"));
        assert!(lines[3].contains("drafted the design"));
    }

    #[test]
    fn status_card_escapes_hostile_artifact_names() {
        // Names are agent-chosen free text; brackets must not break the link
        // Markdown and the URL segment must be percent-encoded.
        let card = render_status_card("http://loom/s/abc", &["a](x) [b".to_string()], &[]);
        assert_eq!(
            card.lines().nth(1).unwrap(),
            "[a\\](x) \\[b](http://loom/s/abc/artifacts/a%5D%28x%29%20%5Bb)"
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
