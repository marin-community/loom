use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use weaver_api::operations::repos as ops;
use weaver_api::{RecentRepoView, RepoBranchView, RepoRevisionValidationView, RepoView};

use crate::backend;
use crate::git;
use crate::github_trigger;
use crate::repo;
use crate::session::{self as session_mod, Session};
use weaver_core::branch as branch_mod;

use super::auth::public_base;
use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};
use crate::lifecycle::auto_archive;
use weaver_api::operations::sessions;

// ---------------------------------------------------------------------------
// Recent repositories
// ---------------------------------------------------------------------------

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
/// request, a replay, an unauthorized requester, a non-allowlisted repo, a
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

    // 3. Normalize new requests, body edits, and submitted reviews. Other event
    //    kinds and actions (including deletes and setup pings) are acknowledged
    //    and ignored.
    let event_kind = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let event = match github_trigger::TriggerEvent::parse(event_kind, &body) {
        Ok(Some(event)) => event,
        Ok(None) => return ok(),
        Err(e) => {
            tracing::warn!(event = event_kind, error = %e, "github webhook: unparseable payload");
            return ok();
        }
    };

    // 4. Ignore the bot's own requests (no self-trigger loop), then require the
    //    configured trigger phrase.
    let author = event.author().trim().to_string();
    if let Some(bot) = github_trigger::bot_login(&st.db).await {
        if author.eq_ignore_ascii_case(&bot) {
            return ok();
        }
    }
    let phrase = github_trigger::trigger_phrase(&st.db).await;
    if !event.introduces_trigger(&phrase) {
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

    // 5. Rate-limit per repo BEFORE authorization, so a request flood cannot
    //    fan out into unbounded launches and replies.
    if !st.trigger.check_rate_limit(&slug.slug()) {
        tracing::warn!(repo = %slug.slug(), "github webhook: per-repo rate limit hit, dropping");
        return ok();
    }

    // 6. Authorize the requester (the untrusted boundary): they must be an
    //    approved loom user — the same allowlist that gates signing in to the app.
    //    Repo write access is *not* itself a grant. Unauthorized → reply once with
    //    a friendly "request access" note instead of a silent drop, so a would-be
    //    user knows to ask rather than assume loom is broken. The per-repo rate
    //    limit above bounds this against a trigger flood; the reply is spawned (a
    //    comment post is a round-trip) and tracked so the attempt shows on Debug.
    if !github_trigger::authorize(&st.db, &author).await {
        tracing::info!(login = %author, repo = %slug.slug(), "github webhook: requester not authorized; replying with access info");
        let number = event.issue.number;
        let slug_str = slug.slug();
        let task_id = crate::tasks::registry().start(
            "github-unauthorized",
            &format!("{slug_str}#{number} (@{author})"),
        );
        weaver_core::spawn_boxed(Box::pin(async move {
            let body = format!(
                "Hi @{author} — thanks for the ping. You're not on this loom instance's \
                 access list yet, so I can't pick this up. Ask an operator to grant you \
                 access, then tag me again and I'll jump in."
            );
            match st
                .trigger
                .gh()
                .post_issue_comment(&slug_str, number, &body)
                .await
            {
                Ok(_) => crate::tasks::registry().finish(task_id, "done", "replied: needs access"),
                Err(e) => {
                    tracing::warn!(repo = %slug_str, error = %e, "github webhook: posting access-info reply failed");
                    crate::tasks::registry().finish(
                        task_id,
                        "error",
                        &format!("reply failed: {e}"),
                    );
                }
            }
        }));
        return ok();
    }

    // 6b. Accept the trigger and hand the heavy work — clone, branch resolution,
    //     session create, reply — to a detached task. That work can take far
    //     longer than GitHub's ~10s webhook timeout on a large repo; doing it
    //     inline lets GitHub drop the connection and cancel the handler mid-clone
    //     (and, since the delivery is already recorded, never retry). So log the
    //     receipt (the Debug stream shows the hook firing), return `200` now, and
    //     run it in the background, tracked in the task registry for the Debug page.
    let number = event.issue.number;
    tracing::info!(
        repo = %slug.slug(),
        number,
        login = %author,
        is_pr = event.issue.is_pr(),
        "github webhook: trigger accepted, launching in background"
    );
    let task_id = crate::tasks::registry().start(
        "github-trigger",
        &format!("{}#{number} (@{author})", slug.slug()),
    );
    weaver_core::spawn_boxed(Box::pin(async move {
        match handle_trigger(st, headers, slug, event, author, phrase).await {
            Ok(Some(id)) => {
                crate::tasks::registry().finish(task_id, "done", &format!("session {id}"))
            }
            Ok(None) => {
                crate::tasks::registry().finish(task_id, "done", "forwarded to existing session")
            }
            Err(e) => crate::tasks::registry().finish(task_id, "error", &e),
        }
    }));
    ok()
}

/// The heavy half of a `@loom` trigger, run detached from the webhook request so
/// a slow clone can't blow the delivery timeout: acquire the clone, resolve the
/// target branch, forward-or-create the session, and reply on the thread. Returns
/// the new session id (`Some`), `None` when the comment was forwarded to an
/// existing session, or an `Err` describing why nothing launched — the string is
/// surfaced on the Debug page's task list.
async fn handle_trigger(
    st: AppState,
    headers: HeaderMap,
    slug: repo::RepoSlug,
    event: github_trigger::TriggerEvent,
    author: String,
    phrase: String,
) -> Result<Option<String>, String> {
    // Honor the App's installation as the repo grant: auto-register any repo the
    // App is installed on into the managed allowlist, so the clone path below
    // accepts it, *complementing* explicitly registered repos. A no-op when the
    // App is unconfigured, the repo is already registered, or the App is not
    // installed on it (leaving the repos-table allowlist to govern).
    if let Some(app) = st.trigger.app() {
        app.ensure_installed_repo_registered(&slug).await;
    }

    // Resolve the requester to their loom user (proven to exist by `authorize`).
    // Attribution governs ownership and audit and selects this user's optional
    // Loom-stored Account PAT for the interactive session. Without one, the
    // selected profile's allowlisted App access is used.
    let username = match crate::auth::user_by_github(&st.db, &author).await {
        Ok(Some(u)) => u.username,
        _ => {
            tracing::warn!(login = %author, "github webhook: approved user vanished before launch");
            return Err("approved user vanished before launch".to_string());
        }
    };

    // Acquire the managed clone (allowlist-gated; `resolve_clone` also fetches
    // `--all`, so a PR's head lands as `origin/<ref>`), then resolve the branch
    // this trigger targets: a PR works on its own head branch so the agent's
    // commits land on the PR; an issue gets a stable `weaver/issue-<n>`.
    let repo_root = match repo::resolve_clone(&st.db, &slug.slug(), st.trigger.app()).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(repo = %slug.slug(), error = ?e, "github webhook: clone/allowlist rejected");
            return Err(format!("clone/allowlist rejected: {e:?}"));
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
    //    request into it rather than spawning a duplicate — unless its terminal is
    //    unreachable, in which case retire it and fall through to a fresh launch
    //    (below) so the request isn't dropped.
    if let Some(branch) = target_branch.as_deref() {
        if let Ok(Some(b)) = branch_mod::find_by_repo_branch(&st.db, &repo_root_str, branch).await {
            if let Ok(Some(sess)) = session_mod::active_for_branch(&st.db, &b.id).await {
                if forward_trigger_to_session(
                    &sess,
                    &author,
                    is_pr,
                    number,
                    event.request_body(),
                    event.source(),
                    &phrase,
                )
                .await
                {
                    crate::events::record(
                        &st.db,
                        &st.bus,
                        &b.id,
                        "nudge",
                        serde_json::json!({ "by": format!("github ({author})"), "text": event.request_body() }),
                    )
                    .await
                    .ok();
                    // Acknowledge quietly: a 👀 reaction on the triggering
                    // comment says "seen, forwarded" right where it was typed,
                    // without an ack comment per mention piling up on the
                    // thread. Fall back to the old ack comment if the reaction
                    // can't land (no comment id, or an App installation that
                    // predates the reactions permission grant) — feedback that
                    // the note reached the session matters more than quiet.
                    let reacted = if let Some(comment_id) = event.comment_id() {
                        st.trigger
                            .gh()
                            .react_to_comment(&slug.slug(), comment_id, "eyes")
                            .await
                            .map_err(|e| {
                                tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: reacting to forwarded request failed");
                            })
                            .is_ok()
                    } else {
                        false
                    };
                    if !reacted {
                        let base = public_base(&st, &headers).await;
                        let reply = format!(
                            "Passed your note to the session already on this thread — {}",
                            crate::links::session_url(&base, &sess.id)
                        );
                        if let Err(e) = st
                            .trigger
                            .gh()
                            .post_issue_comment(&slug.slug(), number, &reply)
                            .await
                        {
                            tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: posting forward-ack failed");
                        }
                    }
                    tracing::info!(session = %sess.id, repo = %slug.slug(), number, "github webhook: forwarded request to active session");
                    return Ok(None);
                }
                // The session is active in the DB but its terminal is unreachable —
                // an orphaned session that outlived its terminal (e.g. a crash the
                // monitor hasn't marked yet). Archive it — the shared teardown
                // captures its chatlog and frees the branch slot (archived no
                // longer counts as active) — then fall through to launch a fresh
                // session: the trigger goal already carries this request and the
                // dead session's commits are on the branch, so nothing is dropped.
                // The fresh launch re-provisions the branch's worktree (see
                // `existing_branch`).
                tracing::warn!(
                    session = %sess.id,
                    branch = %b.id,
                    repo = %slug.slug(),
                    "github webhook: session terminal unreachable; archiving it and launching a fresh session"
                );
                match auto_archive(&st, &sess, &b).await {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Err(format!(
                            "session {} is unreachable and automatic archive is disabled",
                            sess.id
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(session = %sess.id, error = ?e, "github webhook: archiving unreachable session failed");
                        return Err(format!(
                            "archiving unreachable session {} failed: {e:?}",
                            sess.id
                        ));
                    }
                }
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
    let profile = weaver_core::config::get_or(
        &st.db,
        "github.profile",
        weaver_core::config::DEFAULT_GITHUB_PROFILE,
    )
    .await;
    let mut req = sessions::launch::Input {
        repo: Some(slug.slug()),
        title: Some(event.issue.title.clone()),
        goal: Some(trigger_goal(&slug.slug(), is_pr, number, &event, &author)),
        profile: Some(profile),
        // Record the thread on the tracking issue too (issues only — a PR
        // number in the issue link would read as the wrong thing), so the
        // tracking issue's `github_issue` field and the `github` wiring tag
        // agree from birth.
        github_issue: (!is_pr).then_some(number),
        ..Default::default()
    };
    if let Some(branch) = target_branch {
        if is_pr || branch_exists_locally {
            req.existing_branch = Some(branch);
        } else {
            req.name = Some(format!("issue-{number}"));
        }
    }
    let actor = crate::provision::Actor::producer("github", username);
    let created = match crate::provision::create(st.clone(), req, actor).await {
        Ok(created) => created,
        Err(e) => {
            tracing::warn!(repo = %slug.slug(), error = ?e, "github webhook: session create failed");
            return Err(format!("session create failed: {e:?}"));
        }
    };

    // 11. Wire the branch to the thread: the `github` tag is what
    //     `github::sync_status_comment` reads to mirror every `loom status`
    //     write back here. Stamped before the reply so a failed reply still
    //     wires — the first status write then posts the card instead. Left
    //     untouched when already wired to this thread (a relaunch): the tag's
    //     `set_at` scopes the mirrored trail, and re-stamping would truncate it.
    let wired_to = format!("{}#{number}", slug.slug());
    let already_wired = matches!(
        weaver_core::tags::get(&st.db, &created.branch.id, crate::github::WIRED_TAG).await,
        Ok(Some(ref t)) if t.value == wired_to
    );
    if !already_wired {
        weaver_core::tags::set(
            &st.db,
            &created.branch.id,
            crate::github::WIRED_TAG,
            &wired_to,
            "wired by the @loom trigger",
            "loom",
        )
        .await
        .ok();
    }

    // 12. Reply on the thread with the live session URL. The reply is the
    //     session's **status card**: its comment id is recorded so later status
    //     writes edit it in place into the live trail.
    let base = public_base(&st, &headers).await;
    let reply = format!(
        "On it — {}",
        crate::links::session_url(&base, &created.session.id)
    );
    match st
        .trigger
        .gh()
        .post_issue_comment(&slug.slug(), number, &reply)
        .await
    {
        Ok(comment_id) => {
            // A relaunch on an already-wired thread leaves the previous card
            // frozen mid-arc — point its readers at the live one. The full
            // trail re-renders onto the new card (the wiring's `set_at`
            // predates it), so nothing is lost.
            if let Ok(Some(prev)) = weaver_core::tags::get(
                &st.db,
                &created.branch.id,
                crate::github::STATUS_COMMENT_TAG,
            )
            .await
            {
                if prev.note == wired_to {
                    if let Ok(prev_id) = prev.value.parse::<i64>() {
                        if prev_id != comment_id {
                            st.trigger
                                .gh()
                                .update_issue_comment(
                                    &slug.slug(),
                                    prev_id,
                                    "~~On it~~ — this session was relaunched; the live status card is below.",
                                )
                                .await
                                .ok();
                        }
                    }
                }
            }
            crate::github::record_status_comment(
                &st.db,
                &created.branch.id,
                &slug.slug(),
                number,
                comment_id,
            )
            .await;
            // On a relaunch, render the card now (it re-reads the trail), so
            // the new card doesn't sit bare until the next status write. A
            // fresh launch has no trail yet — the reply already is the card.
            if already_wired {
                weaver_core::spawn_boxed(Box::pin(crate::github::sync_status_comment(
                    st.clone(),
                    created.branch.id.clone(),
                )));
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, repo = %slug.slug(), "github webhook: posting reply failed");
        }
    }
    tracing::info!(
        session = %created.session.id,
        repo = %slug.slug(),
        number,
        is_pr,
        login = %author,
        "github webhook: launched session"
    );
    // A PR's `On it` comment already carries the session's loom URL, so mark the
    // branch linked and the poll loop's back-link poster leaves it alone. An
    // issue's `On it` sits on the issue, not the eventual PR, so it doesn't count
    // — the poster links the PR when it opens.
    if is_pr {
        weaver_core::tags::set(
            &st.db,
            &created.branch.id,
            crate::github::LINKED_TAG,
            &created.session.id,
            "loom back-link posted with the trigger reply",
            "loom",
        )
        .await
        .ok();
    }
    Ok(Some(created.session.id))
}

/// Build the opening goal for a trigger-launched session from its thread context
/// and the request that matched the trigger phrase.
fn trigger_goal(
    repo: &str,
    is_pr: bool,
    number: i64,
    event: &github_trigger::TriggerEvent,
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
    let comment_cmd = if is_pr { "pr" } else { "issue" };
    let respond = format!(
        "- Post the final answer or completed result on the thread: `gh {comment_cmd} comment {number} --repo {repo} --body \"…\"`.\n\
         - Your `loom status` messages are mirrored onto this thread. When you create or update a pull request or issue, or otherwise reach a terminal outcome, replace any transient progress such as `waiting` with a final status that names the outcome and includes its URL when available. Use `attention` when a person needs to review or act; otherwise use `ok`.\n\
         - Do not leave the thread with only the editable status card: the final GitHub comment must be self-contained."
    );
    let (introduction, trigger_context) = match event.source() {
        github_trigger::TriggerSource::CommentCreated => (
            format!("You've been tagged into GitHub {kind} #{number} of {repo} ({url}) via a comment."),
            format!(
                "\n\n## Triggering comment (from @{author})\n{}",
                event.request_body().trim()
            ),
        ),
        github_trigger::TriggerSource::CommentEdited => (
            format!(
                "You've been tagged into GitHub {kind} #{number} of {repo} ({url}) via an edited comment by @{author}."
            ),
            format!(
                "\n\n## Triggering edited comment\n{}",
                event.request_body().trim()
            ),
        ),
        github_trigger::TriggerSource::IssueOpened => (
            format!(
                "You've been tagged into GitHub {kind} #{number} of {repo} ({url}) via its body, opened by @{author}."
            ),
            String::new(),
        ),
        github_trigger::TriggerSource::IssueEdited => (
            format!(
                "You've been tagged into GitHub {kind} #{number} of {repo} ({url}) via its body, edited by @{author}."
            ),
            String::new(),
        ),
        github_trigger::TriggerSource::PullRequestReviewSubmitted => (
            format!(
                "You've been tagged into GitHub {kind} #{number} of {repo} ({url}) via a submitted review by @{author}."
            ),
            format!(
                "\n\n## Triggering review\n{}",
                event.request_body().trim()
            ),
        ),
    };
    format!(
        "{introduction}\n\n## {title_kind}\n{}\n\n{body}{trigger_context}\n\n## How to respond\n{respond}",
        event.issue.title.trim(),
    )
}

/// Inject a new GitHub trigger into an already-running session's terminal so the
/// thread continues in one session. Returns whether the note was delivered.
async fn forward_trigger_to_session(
    session: &Session,
    author: &str,
    is_pr: bool,
    number: i64,
    request: &str,
    source: github_trigger::TriggerSource,
    phrase: &str,
) -> bool {
    let (thread, cmd) = if is_pr {
        ("PR", "pr")
    } else {
        ("issue", "issue")
    };
    let source = match source {
        github_trigger::TriggerSource::CommentCreated => "comment",
        github_trigger::TriggerSource::CommentEdited => "edited comment",
        github_trigger::TriggerSource::IssueOpened => "request in the issue body",
        github_trigger::TriggerSource::IssueEdited => "edited request in the issue body",
        github_trigger::TriggerSource::PullRequestReviewSubmitted => "submitted review",
    };
    let note = format!(
        "New {phrase} {source} from @{author} on {thread} #{number}:\n\n{}\n\n\
         (Reply on the thread with `gh {cmd} comment {number} --body \"…\"` if a response is warranted.)",
        request.trim(),
    );
    if let Err(e) = backend::paste(&session.term_session, &note).await {
        tracing::warn!(session = %session.id, error = %e, "github webhook: forwarding request to session failed");
        return false;
    }
    if let Err(e) = backend::send_enter(&session.term_session).await {
        tracing::warn!(session = %session.id, error = %e, "github webhook: submitting forwarded request failed");
    }
    true
}

// ---------------------------------------------------------------------------
// Operation registry — `repos.*`, bound onto `weaver_api::operations::repos`.
// Authorization (`actor = User`, `scope = Global`) happens once, centrally,
// in `web/operations.rs`.
//
// `repos.env.*` binds handlers that live in `repo_env.rs`; its
// `bound_operations()` is folded into this bundle's, since the coordinator
// only calls `repos::bound_operations()`.
// ---------------------------------------------------------------------------

fn repo_view(r: repo::ManagedRepo) -> RepoView {
    RepoView {
        slug: r.slug,
        remote_url: r.remote_url,
        path: r.path,
        created_at: r.created_at,
    }
}

pub(super) fn bound_operations() -> Vec<Bound> {
    let mut bound = vec![
        register::<ops::list::Op, _, _>(list_operation),
        register::<ops::register::Op, _, _>(register_operation),
        register::<ops::recent::Op, _, _>(recent_operation),
        register::<ops::branches::Op, _, _>(branches_operation),
        register::<ops::revisions::validate::Op, _, _>(revisions_validate_operation),
    ];
    bound.extend(super::repo_env::bound_operations());
    bound
}

async fn list_operation(
    context: OperationContext,
    _input: ops::list::Input,
) -> ApiResult<ops::list::Output> {
    let st = context.state;
    let repos = repo::list_registered(&st.db).await?;
    Ok(repos.into_iter().map(repo_view).collect())
}

async fn register_operation(
    context: OperationContext,
    input: ops::register::Input,
) -> ApiResult<ops::register::Output> {
    let st = context.state;
    let slug = repo::parse_slug(&input.repo).map_err(AppError::bad_request)?;
    let remote_url = repo::remote_url_for(&input.repo, &slug);
    let path = slug.path(&repo::repos_dir());
    let managed =
        repo::register(&st.db, &slug.slug(), &remote_url, &path.to_string_lossy()).await?;
    Ok(repo_view(managed))
}

async fn recent_operation(
    context: OperationContext,
    input: ops::recent::Input,
) -> ApiResult<ops::recent::Output> {
    let st = context.state;
    let limit = input.limit.unwrap_or(10).clamp(1, 50);
    let repos = repo::recent(&st.db, limit).await?;
    Ok(repos
        .into_iter()
        .map(|r| RecentRepoView {
            repo_root: r.repo_root,
            last_used_at: r.last_used_at,
            active_branches: r.active_branches,
        })
        .collect())
}

/// `repos.branches`. Server-local: `cwd` is a filesystem path the server
/// process can read, not a session's own scope, so this ignores `context`
/// entirely (`scope = Global`, `actor = User`).
async fn branches_operation(
    _context: OperationContext,
    input: ops::branches::Input,
) -> ApiResult<ops::branches::Output> {
    let cwd = PathBuf::from(&input.cwd);
    let repo_root = git::repo_root(&cwd)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let current = git::current_branch(&repo_root).await.ok();
    let names = git::list_branches(&repo_root).await?;
    let mut out: Vec<RepoBranchView> = Vec::with_capacity(names.len());
    for name in names {
        let worktree = git::worktree_for_branch(&repo_root, &name)
            .await
            .ok()
            .flatten()
            .map(|p| p.display().to_string());
        let is_current = current.as_deref() == Some(name.as_str());
        out.push(RepoBranchView {
            name,
            worktree,
            current: is_current,
        });
    }
    out.sort_by(|a, b| {
        let rank = |b: &RepoBranchView| {
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
    Ok(out)
}

async fn revisions_validate_operation(
    _context: OperationContext,
    input: ops::revisions::validate::Input,
) -> ApiResult<ops::revisions::validate::Output> {
    let cwd = PathBuf::from(&input.cwd);
    let repo_root = git::repo_root(&cwd)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let revision = input.revision.trim();
    let valid = git::resolve_base(&repo_root, revision).await.is_some();
    let message = (!valid).then(|| git::missing_revision_message(&repo_root, revision));
    Ok(RepoRevisionValidationView {
        valid,
        repo_root: repo_root.display().to_string(),
        message,
    })
}
