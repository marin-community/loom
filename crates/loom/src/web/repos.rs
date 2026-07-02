use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use weaver_api::CreateReq;

use crate::backend;
use crate::git;
use crate::github_trigger;
use crate::repo;
use crate::session::{self as session_mod, Session};
use weaver_core::branch as branch_mod;

use super::auth::external_base;
use super::sessions::create_session_core;
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Recent repositories
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct RecentReposQuery {
    limit: Option<i64>,
}

pub(super) async fn recent_repos(
    State(st): State<AppState>,
    Query(q): Query<RecentReposQuery>,
) -> ApiResult<Json<Vec<repo::RecentRepo>>> {
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    Ok(Json(repo::recent(&st.db, limit).await?))
}

/// `GET /api/repos` — the registered managed repos (the clone allowlist).
pub(super) async fn list_repos(
    State(st): State<AppState>,
) -> ApiResult<Json<Vec<repo::ManagedRepo>>> {
    Ok(Json(repo::list_registered(&st.db).await?))
}

/// Body for `POST /api/repos`: a repo reference — a GitHub `owner/name` slug or a
/// clone URL — to add to the managed store / allowlist.
#[derive(Debug, Deserialize)]
pub(super) struct RegisterRepoReq {
    repo: String,
}

/// `POST /api/repos` — register a repo in the managed store. The reference is
/// parsed to a clean `owner/name` slug (traversal rejected → 400); the clone URL
/// is the canonical GitHub HTTPS remote for a bare slug, or the URL as given.
/// The clone itself is lazy — it happens on first use (session create),
/// idempotently — so registering is just adding to the allowlist.
pub(super) async fn register_repo(
    State(st): State<AppState>,
    Json(req): Json<RegisterRepoReq>,
) -> ApiResult<Json<repo::ManagedRepo>> {
    let slug = repo::parse_slug(&req.repo).map_err(AppError::bad_request)?;
    let remote_url = repo::remote_url_for(&req.repo, &slug);
    let path = slug.path(&repo::repos_dir());
    let managed =
        repo::register(&st.db, &slug.slug(), &remote_url, &path.to_string_lossy()).await?;
    Ok(Json(managed))
}

/// `POST /api/github/webhook` — the inbound GitHub trigger (shared-loom design
/// §6.3). **Public** (outside `require_auth`): every delivery is authenticated by
/// the HMAC signature GitHub carries on it, not by a loom principal. This handler
/// is the untrusted-input boundary; it sequences the gates implemented in
/// [`crate::github_trigger`].
///
/// Status discipline: a missing/invalid signature is a hard **401** (a real
/// misconfiguration GitHub should surface as a failed delivery). Two further
/// non-2xx cases past that are deliberate, not no-ops: a delivery with no
/// `X-GitHub-Delivery` GUID is malformed (**400** — without it idempotency is
/// impossible), and a failure to record the delivery is transient (**5xx**, so
/// GitHub *should* retry). Every *business-logic* outcome — a non-trigger
/// comment, a replay, an unauthorized commenter, a non-allowlisted repo, a
/// rate-limited repo — returns **200**, so GitHub does not retry a delivery we
/// deliberately ignored.
pub(super) async fn github_webhook(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 1. Authenticate the delivery: HMAC-SHA256 over the RAW body bytes (never a
    //    re-serialized parse). An empty secret means the webhook is unconfigured,
    //    so it cannot verify anything — reject.
    let secret = github_trigger::webhook_secret(&st.db).await;
    if secret.is_empty() {
        tracing::warn!("github webhook hit but no webhook secret is configured");
        return (StatusCode::UNAUTHORIZED, "webhook not configured").into_response();
    }
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok());
    if !github_trigger::verify_signature(&secret, &body, sig) {
        tracing::warn!("github webhook signature verification failed");
        return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }

    // The body is now trusted (GitHub-signed). Every *business-logic* outcome
    // past here is a 200 no-op via `ok()`; the only non-2xx exceptions below are
    // a malformed delivery (no GUID → 400) and a transient store error (→ 5xx).
    let ok = || (StatusCode::OK, "ok").into_response();

    // 2. Idempotency: dedupe on the delivery GUID. A genuine GitHub delivery
    //    always carries one; its absence is a malformed request we reject (400),
    //    since without it idempotency is impossible. A repeat GUID is a no-op.
    let Some(delivery) = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("github webhook missing X-GitHub-Delivery");
        return (StatusCode::BAD_REQUEST, "missing delivery id").into_response();
    };
    match github_trigger::record_delivery(&st.db, delivery).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(delivery, "github webhook: duplicate delivery ignored");
            return ok();
        }
        Err(e) => {
            tracing::error!(error = %e, "github webhook: recording delivery failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "delivery store error").into_response();
        }
    }

    // 3. Filter to issue_comment / created. Other events (incl. the setup `ping`)
    //    and edited/deleted comments are acknowledged and ignored.
    if headers.get("x-github-event").and_then(|v| v.to_str().ok()) != Some("issue_comment") {
        return ok();
    }
    let event = match github_trigger::IssueCommentEvent::parse(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "github webhook: unparseable issue_comment payload");
            return ok();
        }
    };
    if event.action != "created" {
        return ok();
    }

    // 4. Ignore the bot's own comments (no self-trigger loop), then require the
    //    fixed command prefix.
    let author = event.comment.user.login.trim().to_string();
    if let Some(bot) = github_trigger::bot_login(&st.db).await {
        if author.eq_ignore_ascii_case(&bot) {
            return ok();
        }
    }
    let phrase = github_trigger::trigger_phrase(&st.db).await;
    if !github_trigger::is_trigger(&event.comment.body, &phrase) {
        return ok();
    }

    // Validate the repo identifier (defence — it is GitHub's, but the on-disk
    // path derives from it) and split it into owner/name.
    let slug = match repo::parse_slug(&event.repository.full_name) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(repo = %event.repository.full_name, error = %e, "github webhook: bad repo slug");
            return ok();
        }
    };

    // 5. Rate-limit per repo BEFORE the (costly) authorization API call, so a
    //    comment flood cannot fan out into unbounded GitHub calls and launches.
    if !st.trigger.check_rate_limit(&slug.slug()) {
        tracing::warn!(repo = %slug.slug(), "github webhook: per-repo rate limit hit, dropping");
        return ok();
    }

    // 6. Authorize the commenter (the untrusted boundary): they must be an
    //    approved loom user — the same allowlist that gates signing in to the app.
    //    Repo write access is *not* itself a grant. Unauthorized → silent no-op;
    //    replying would amplify spam across a flood of comments.
    if !github_trigger::authorize(&st.db, &author).await {
        tracing::info!(login = %author, repo = %slug.slug(), "github webhook: commenter not authorized");
        return ok();
    }

    // 6a. An approved user has been authorized above, so honor the App's
    //     installation as the repo grant: auto-register any repo the App is
    //     installed on into the managed allowlist, so the clone path below accepts
    //     it, *complementing* explicitly registered repos. A no-op when the App is
    //     unconfigured, the repo is already registered, or the App is not installed
    //     on it (leaving the repos-table allowlist to govern).
    if let Some(app) = st.trigger.app() {
        app.ensure_installed_repo_registered(&slug).await;
    }

    // 7. Resolve the commenter to their loom user (proven to exist by `authorize`
    //    above). Attributing the launch to them makes their personal GitHub token
    //    the session's `GH_TOKEN` — so its push / `gh` act as them — with the
    //    ambient token as the fallback (see `apply_user_github_token`).
    let username = match crate::auth::user_by_github(&st.db, &author).await {
        Ok(Some(u)) => u.username,
        _ => {
            tracing::warn!(login = %author, "github webhook: approved user vanished before launch");
            return ok();
        }
    };

    // 8. Acquire the managed clone (allowlist-gated; `resolve_clone` also fetches
    //    `--all`, so a PR's head lands as `origin/<ref>`), then resolve the branch
    //    this trigger targets: a PR works on its own head branch so the agent's
    //    commits land on the PR; an issue gets a stable `weaver/issue-<n>`.
    let repo_root = match repo::resolve_clone(&st.db, &slug.slug(), st.trigger.app()).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(repo = %slug.slug(), error = ?e, "github webhook: clone/allowlist rejected");
            return ok();
        }
    };
    let repo_root_str = repo_root.to_string_lossy().to_string();
    let number = event.issue.number;
    let is_pr = event.issue.is_pr();

    let mut target_branch = if is_pr {
        match st.trigger.gh().pr_head(&slug.slug(), number).await {
            // A fork PR's head is unreachable/unpushable — fall through to a fresh
            // auto-named branch rather than pretend to attach to it.
            Ok(h) if h.cross_repo => {
                tracing::info!(repo = %slug.slug(), pr = number, "cross-repo PR; using a fresh branch");
                None
            }
            Ok(h) => Some(h.head_ref),
            Err(e) => {
                tracing::warn!(repo = %slug.slug(), pr = number, error = %e, "github webhook: PR head lookup failed");
                None
            }
        }
    } else {
        Some(format!("weaver/issue-{number}"))
    };

    // Materialize a PR head branch locally — bare names resolve only local heads,
    // so `existing_branch` needs a real `refs/heads/<ref>`. On failure, drop to a
    // fresh branch.
    if is_pr {
        if let Some(branch) = target_branch.clone() {
            if let Err(e) = git::create_local_branch_from_origin(&repo_root, &branch).await {
                tracing::warn!(repo = %slug.slug(), %branch, error = %e, "github webhook: could not materialize PR branch");
                target_branch = None;
            }
        }
    }

    // 9. If an active session already owns the target branch, forward the new
    //    comment into it rather than spawning a duplicate.
    if let Some(branch) = target_branch.as_deref() {
        if let Ok(Some(b)) = branch_mod::find_by_repo_branch(&st.db, &repo_root_str, branch).await {
            if let Ok(Some(sess)) = session_mod::active_for_branch(&st.db, &b.id).await {
                if forward_comment_to_session(&sess, &author, is_pr, number, &event.comment.body)
                    .await
                {
                    crate::events::record(
                        &st.db,
                        &st.bus,
                        &b.id,
                        "nudge",
                        serde_json::json!({ "by": format!("github ({author})"), "text": event.comment.body }),
                    )
                    .await
                    .ok();
                    let base = external_base(&st, &headers)
                        .await
                        .unwrap_or_else(|| format!("http://{}", st.addr));
                    let reply = format!(
                        "Passed your note to the session already on this thread — {base}/s/{}",
                        sess.id
                    );
                    if let Err(e) = st
                        .trigger
                        .gh()
                        .post_issue_comment(&slug.slug(), number, &reply)
                        .await
                    {
                        tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: posting forward-ack failed");
                    }
                    tracing::info!(session = %sess.id, repo = %slug.slug(), number, "github webhook: forwarded comment to active session");
                }
                return ok();
            }
        }
    }

    // 10. Otherwise create a new session. A PR (or a dormant issue branch that
    //     already exists) attaches to that branch so work lands on it; a first-time
    //     issue creates `weaver/issue-<n>`; a fork PR / lookup failure auto-names.
    let branch_exists_locally = match target_branch.as_deref() {
        Some(b) => git::branch_exists(&repo_root, b).await,
        None => false,
    };
    let mut req = CreateReq {
        repo: Some(slug.slug()),
        title: Some(event.issue.title.clone()),
        goal: Some(trigger_goal(&slug.slug(), is_pr, number, &event, &author)),
        ..Default::default()
    };
    if let Some(branch) = target_branch {
        if is_pr || branch_exists_locally {
            req.existing_branch = Some(branch);
        } else {
            req.name = Some(format!("issue-{number}"));
        }
    }
    let view = match create_session_core(st.clone(), req, Some(username)).await {
        Ok(v) => v,
        Err(e) => {
            // A non-allowlisted repo lands here (a BadRequest from resolve_clone),
            // as does a clone/launch failure. Acknowledge; don't make GitHub retry.
            tracing::warn!(repo = %slug.slug(), error = ?e, "github webhook: session create failed");
            return ok();
        }
    };

    // 11. Reply on the thread with the live session URL.
    let base = external_base(&st, &headers)
        .await
        .unwrap_or_else(|| format!("http://{}", st.addr));
    let reply = format!("On it — {base}/s/{}", view.id);
    if let Err(e) = st
        .trigger
        .gh()
        .post_issue_comment(&slug.slug(), number, &reply)
        .await
    {
        tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: posting reply failed");
    }
    tracing::info!(
        session = %view.id,
        repo = %slug.slug(),
        number,
        is_pr,
        login = %author,
        "github webhook: launched session"
    );
    ok()
}

/// Build the opening goal for a trigger-launched session: the issue/PR title and
/// body, the triggering comment, and how to respond — push to the PR branch (or
/// open a PR) and reply on the thread with `gh`.
fn trigger_goal(
    repo: &str,
    is_pr: bool,
    number: i64,
    event: &github_trigger::IssueCommentEvent,
    author: &str,
) -> String {
    let (kind, title_kind, url) = if is_pr {
        (
            "pull request",
            "Pull request",
            format!("https://github.com/{repo}/pull/{number}"),
        )
    } else {
        (
            "issue",
            "Issue",
            format!("https://github.com/{repo}/issues/{number}"),
        )
    };
    let body = event
        .issue
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .unwrap_or("(no description)");
    let respond = if is_pr {
        format!(
            "- This worktree is checked out on the PR's own branch — commit and `git push` here to update pull request #{number} directly.\n\
             - Reply on the thread when you have something to report: `gh pr comment {number} --repo {repo} --body \"…\"`."
        )
    } else {
        format!(
            "- Do the work on this branch and open a pull request against the default branch when it's ready.\n\
             - Reply on the thread when you have something to report: `gh issue comment {number} --repo {repo} --body \"…\"`."
        )
    };
    format!(
        "You've been tagged into GitHub {kind} #{number} of {repo} ({url}) via a comment.\n\n\
         ## {title_kind}\n{}\n\n{body}\n\n\
         ## Triggering comment (from @{author})\n{}\n\n\
         ## How to respond\n{respond}",
        event.issue.title.trim(),
        event.comment.body.trim(),
    )
}

/// Inject a "new comment" note into an already-running session's terminal so a
/// follow-up @loom comment continues the existing thread instead of forking a new
/// session. Returns whether the note was delivered (best-effort: a dead terminal
/// — e.g. an orphaned session — logs and returns `false`).
async fn forward_comment_to_session(
    session: &Session,
    author: &str,
    is_pr: bool,
    number: i64,
    comment: &str,
) -> bool {
    let (thread, cmd) = if is_pr {
        ("PR", "pr")
    } else {
        ("issue", "issue")
    };
    let note = format!(
        "New @loom comment from @{author} on {thread} #{number}:\n\n{}\n\n\
         (Reply on the thread with `gh {cmd} comment {number} --body \"…\"` if a response is warranted.)",
        comment.trim(),
    );
    if let Err(e) = backend::send_literal(&session.term_session, &note).await {
        tracing::warn!(session = %session.id, error = %e, "github webhook: forwarding comment to session failed");
        return false;
    }
    if let Err(e) = backend::send_enter(&session.term_session).await {
        tracing::warn!(session = %session.id, error = %e, "github webhook: submitting forwarded comment failed");
    }
    true
}

#[derive(Debug, Deserialize)]
pub(super) struct BranchesQuery {
    cwd: String,
}

#[derive(Debug, Serialize)]
pub(super) struct BranchInfo {
    name: String,
    worktree: Option<String>,
    current: bool,
}

pub(super) async fn repo_branches(
    Query(q): Query<BranchesQuery>,
) -> ApiResult<Json<Vec<BranchInfo>>> {
    let cwd = PathBuf::from(&q.cwd);
    let repo_root = git::repo_root(&cwd)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let current = git::current_branch(&repo_root).await.ok();
    let names = git::list_branches(&repo_root).await?;
    let mut out: Vec<BranchInfo> = Vec::with_capacity(names.len());
    for name in names {
        let worktree = git::worktree_for_branch(&repo_root, &name)
            .await
            .ok()
            .flatten()
            .map(|p| p.display().to_string());
        let is_current = current.as_deref() == Some(name.as_str());
        out.push(BranchInfo {
            name,
            worktree,
            current: is_current,
        });
    }
    out.sort_by(|a, b| {
        let rank = |b: &BranchInfo| {
            if b.current {
                0
            } else if b.worktree.is_some() {
                1
            } else {
                2
            }
        };
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(out))
}
