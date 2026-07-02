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

use crate::git;
use crate::github_trigger;
use crate::repo;

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

    // 7. Acquire the repo (it must be in the managed allowlist — `resolve_clone`
    //    rejects others) and create the session, seeded from the issue carried in
    //    the trusted payload. Seeding title/goal from the payload (not a `gh`
    //    re-fetch) means the managed clone needs no reachable GitHub remote.
    //    Attribute the launch to the webhook bot, annotated with the triggering
    //    GitHub login (`created_by`, from PR #94) — tracking, not a boundary.
    let req = CreateReq {
        repo: Some(slug.slug()),
        title: Some(event.issue.title.clone()),
        goal: Some(event.goal_seed()),
        ..Default::default()
    };
    let created_by = format!("github-webhook ({author})");
    let view = match create_session_core(st.clone(), req, Some(created_by)).await {
        Ok(v) => v,
        Err(e) => {
            // A non-allowlisted repo lands here (a BadRequest from resolve_clone),
            // as does a clone/launch failure. Acknowledge; don't make GitHub retry.
            tracing::warn!(repo = %slug.slug(), error = ?e, "github webhook: session create failed");
            return ok();
        }
    };

    // 8. Reply on the issue with the live session URL.
    let base = external_base(&st, &headers)
        .await
        .unwrap_or_else(|| format!("http://{}", st.addr));
    let reply = format!("On it — {base}/s/{}", view.id);
    if let Err(e) = st
        .trigger
        .gh()
        .post_issue_comment(&slug.slug(), event.issue.number, &reply)
        .await
    {
        tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: posting reply failed");
    }
    tracing::info!(
        session = %view.id,
        repo = %slug.slug(),
        issue = event.issue.number,
        login = %author,
        "github webhook: launched session"
    );
    ok()
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
