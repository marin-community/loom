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
//!   archives a session once its PR merges. The snapshot rides along on
//!   `BranchView`; the dashboard renders it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::db::{now_iso, Db};
use crate::session::{self as session_mod, Session};
use crate::web::AppState;
use crate::{branch as branch_mod, config, events};
use weaver_core::branch::Branch;

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
    gh(
        work_dir,
        &[
            "pr", "create", "--base", base, "--title", title, "--body", body,
        ],
    )
    .await
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

/// The fields requested from `gh pr view --json`. Kept in one place so the parse
/// struct and the query can't drift.
const PR_FIELDS: &str =
    "number,url,state,title,isDraft,reviewDecision,mergeable,mergedAt,statusCheckRollup";

/// A branch's pull-request snapshot, as stored and as served under
/// `BranchView::github`. `pr_state` is `OPEN` / `CLOSED` / `MERGED`; `checks` is
/// the rolled-up `passing` / `failing` / `pending` (or `null` when the PR has no
/// checks); `review_decision` is GitHub's `APPROVED` / `CHANGES_REQUESTED` /
/// `REVIEW_REQUIRED` (or `null` when review isn't required).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct GithubStatus {
    pub pr_number: i64,
    pub pr_url: String,
    pub pr_state: String,
    pub pr_title: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub checks: Option<String>,
    pub mergeable: Option<String>,
    pub merged_at: Option<String>,
    pub fetched_at: String,
}

impl GithubStatus {
    /// The fields whose change is worth announcing on the activity feed — PR
    /// identity and the human-meaningful state. `mergeable` and timestamps are
    /// deliberately excluded: they flap (e.g. `UNKNOWN` ⇄ `MERGEABLE`) without
    /// telling the user anything.
    fn signature(&self) -> (i64, String, Option<String>, Option<String>, bool) {
        (
            self.pr_number,
            self.pr_state.clone(),
            self.review_decision.clone(),
            self.checks.clone(),
            self.is_draft,
        )
    }

    /// Payload for the `github` event the poller records when the snapshot
    /// changes — enough for a dashboard to summarize without re-fetching.
    pub fn event_data(&self) -> Value {
        json!({
            "pr": self.pr_number,
            "url": self.pr_url,
            "state": self.pr_state,
            "review": self.review_decision,
            "checks": self.checks,
            "draft": self.is_draft,
        })
    }
}

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

/// Fetch the pull request for `branch` (its remote head ref) from `repo_root`.
/// `Ok(None)` means there is simply no PR for the branch yet; `Err` is a real
/// failure (no GitHub remote, auth, `gh` missing) the caller logs and skips.
pub async fn fetch_pr(repo_root: &Path, branch: &str) -> Result<Option<GithubStatus>> {
    let out = Command::new("gh")
        .args(["pr", "view", branch, "--json", PR_FIELDS])
        .current_dir(repo_root)
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
        bail!("gh pr view {branch} failed: {}", stderr.trim());
    }
    let raw: PrJson = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .context("parsing gh pr JSON")?;
    Ok(Some(raw.into_status()))
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
/// archive the still-live session. Returns the fresh snapshot, or `None` when
/// the branch has no PR. The single code path behind both the poller and the
/// on-demand refresh endpoint.
pub async fn refresh(
    state: &AppState,
    session: &Session,
    branch: &Branch,
    archive_on_merge: bool,
) -> Result<Option<GithubStatus>> {
    let snap = match fetch_pr(&PathBuf::from(&branch.repo_root), &branch.branch).await? {
        Some(s) => s,
        None => return Ok(None),
    };
    apply_snapshot(state, session, branch, &snap, archive_on_merge).await?;
    Ok(Some(snap))
}

/// Persist a freshly-fetched snapshot, announce a meaningful change on the
/// activity feed, and archive a still-live session whose PR has merged (when
/// `archive_on_merge` is set). Split from [`refresh`] so the storage and
/// merge-archive behaviour is testable without invoking `gh`.
async fn apply_snapshot(
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

    if archive_on_merge && snap.pr_state == "MERGED" && !session_mod::is_terminal(&session.status) {
        // The merge is already on the record as a `github` event (above) and the
        // archive records a `status` event, so no extra log line is needed.
        match crate::web::archive(state, session, branch).await {
            Ok(_) => tracing::info!(
                branch = %branch.branch,
                pr = snap.pr_number,
                "archived session after PR merge"
            ),
            Err(e) => tracing::warn!(
                branch = %branch.branch,
                error = %e.message(),
                "archive-on-merge failed"
            ),
        }
    }
    Ok(())
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
    // sessions visits each candidate branch once.
    for session in session_mod::list(&state.db).await? {
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
                tmux_session: format!("weaver-ghtest-{}", std::process::id()),
                agent_kind: "shell".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
            },
        )
        .await
        .unwrap();
        let state = AppState {
            db,
            bus: events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
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
}
