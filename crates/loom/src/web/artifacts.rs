use std::collections::HashMap;

use axum::{
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde_json::json;
use weaver_api::operations::artifacts::{delete, get, history, list, raw, threads, url, write};
use weaver_api::{
    AnchorDto, ArtifactMeta, ArtifactRefs, ArtifactVersion, ArtifactView, CommentDto,
    IssueRefStatus, SessionUrlView, ThreadDto,
};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
use weaver_core::discussion;

use crate::db::Db;
use crate::events;

use super::operations::{register, Bound, OperationContext};
use super::require_branch;
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Artifacts — named, versioned documents stored in weaver.db. The GET resolves
// the content's references against the repo's issues (via smartdoc) and
// returns the resolved reference map alongside, so the SPA chips and
// `loom artifacts get` render the same join. Structure in the doc, state in
// the DB. See docs/artifacts.md.
// ---------------------------------------------------------------------------

const MAX_ARTIFACT_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Pull a standalone image data URI out of artifact content: the explicit
/// `image` kind stores it directly, the markdown fallback covers artifacts
/// under the older envelope. Kind is current metadata, not versioned, so this
/// also covers old revisions after a kind change.
fn artifact_image_uri(content: &str) -> Option<&str> {
    let content = content.trim();
    if content.starts_with("data:image/") {
        return Some(content);
    }
    if !content.starts_with("![") || !content.ends_with(')') {
        return None;
    }
    let start = content.rfind("](data:image/")? + 2;
    Some(&content[start..content.len() - 1])
}

/// Decode the bounded image data-URI formats accepted by `loom artifacts
/// write`. The MIME whitelist prevents a caller from using an artifact as an
/// arbitrary same-origin response.
fn decode_artifact_image(content: &str) -> Option<(&'static str, Vec<u8>)> {
    let uri = artifact_image_uri(content)?;
    let (metadata, payload) = uri.strip_prefix("data:")?.split_once(',')?;
    let mime = metadata.strip_suffix(";base64")?;
    let mime = match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/svg+xml" => "image/svg+xml",
        "image/avif" => "image/avif",
        "image/bmp" => "image/bmp",
        "image/x-icon" => "image/x-icon",
        _ => return None,
    };
    // Refuse an oversized encoded payload before allocating its decoded form.
    let max_encoded = MAX_ARTIFACT_IMAGE_BYTES.div_ceil(3) * 4;
    if payload.len() > max_encoded {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    (bytes.len() <= MAX_ARTIFACT_IMAGE_BYTES).then_some((mime, bytes))
}

/// The JSON metadata for an artifact envelope.
pub(super) fn artifact_meta(a: &Artifact) -> ArtifactMeta {
    ArtifactMeta {
        id: a.id,
        name: a.name.clone(),
        kind: a.kind.clone(),
        title: a.title.clone(),
        branch_id: a.branch_id.clone(),
        rev: a.rev,
        created_at: a.created_at.clone(),
        updated_at: a.updated_at.clone(),
    }
}

/// Resolve an artifact's content references to their live status, as
/// [`ArtifactRefs`]. Probes each `#N` against the repo's issues and joins via
/// [`smartdoc::project`]; an unresolved reference is omitted from the map.
async fn project_artifact_refs(db: &Db, repo_root: &str, content: &str) -> ArtifactRefs {
    let doc = smartdoc::parse(content);
    // Probe each distinct reference against weaver-core. Best-effort: a probe
    // miss (unknown issue, wrong repo, read error) leaves that ref absent from
    // the status map, which `project` renders as a muted, non-existent chip.
    let mut status: HashMap<smartdoc::Ref, smartdoc::RefStatus> = HashMap::new();
    for r in smartdoc::refs(&doc) {
        if let smartdoc::Ref::Issue(n) = &r {
            if let Ok(Some(issue)) = weaver_core::issue::get(db, *n as i64).await {
                if issue.repo_root == repo_root {
                    status.insert(
                        r.clone(),
                        smartdoc::RefStatus {
                            exists: true,
                            title: issue.title,
                            status: issue.status,
                            claimed_branch: issue.claimed_branch,
                        },
                    );
                }
            }
        }
    }
    // Join, then build the map of resolved issue refs, keyed by id.
    let mut refs = ArtifactRefs::default();
    for pr in smartdoc::project(&doc, &status).refs {
        if let smartdoc::Ref::Issue(n) = pr.reference {
            if pr.status.exists {
                refs.issues.insert(
                    n.to_string(),
                    IssueRefStatus {
                        id: n as i64,
                        title: pr.status.title,
                        status: pr.status.status,
                        claimed_branch: pr.status.claimed_branch,
                    },
                );
            }
        }
    }
    refs
}

/// Build the full [`ArtifactView`] for an artifact at a given revision (default
/// latest): envelope, content, version list, and the resolved reference map.
async fn artifact_view(
    db: &Db,
    repo_root: &str,
    a: &Artifact,
    rev: Option<i64>,
) -> ApiResult<ArtifactView> {
    let version = match rev {
        Some(r) => artifact::version(db, a.id, r).await?,
        None => artifact::latest_version(db, a.id).await?,
    }
    .ok_or_else(|| AppError::not_found("artifact revision"))?;
    let versions = artifact::history(db, a.id)
        .await?
        .into_iter()
        .map(|v| weaver_api::ArtifactVersion {
            rev: v.rev,
            author: v.author,
            created_at: v.created_at,
        })
        .collect();
    let refs = project_artifact_refs(db, repo_root, &version.content).await;
    Ok(ArtifactView {
        meta: artifact_meta(a),
        content: version.content,
        versions,
        refs,
    })
}

/// `artifacts.raw` — an image artifact's decoded bytes for an `<img src>`.
///
/// Markdown documents can use `![alt](artifact:<name>)`; the renderer maps that
/// source to this route, so the browser depends only on loom's artifact store
/// and never on an agent-local path. `?rev=N` pins an older image revision;
/// omitted means latest.
///
/// Mounted by hand in [`super::encodings`] because `Io::Download` answers with
/// bytes and a content type rather than a JSON envelope. Authorization is not
/// hand-written: the caller there runs the same `authorized` path every JSON
/// operation does, and what is left for this function is the bytes.
pub(super) async fn raw_artifact_bytes(st: &AppState, input: &raw::Input) -> ApiResult<Response> {
    let branch = require_branch(&st.db, &input.branch).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, &input.name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    let version = match input.rev {
        Some(rev) => artifact::version(&st.db, a.id, rev).await?,
        None => artifact::latest_version(&st.db, a.id).await?,
    }
    .ok_or_else(|| AppError::not_found("artifact revision"))?;
    let (mime, bytes) = decode_artifact_image(&version.content)
        .ok_or_else(|| AppError::bad_request("artifact is not a valid image"))?;

    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

// ---------------------------------------------------------------------------
// Operation registry — `artifacts.*` and `artifacts.threads.*`, bound onto
// `weaver_api::operations::artifacts`. `branch` resolves to a branch id, a
// branch name, or an active session's branch, and authorization happens once,
// centrally, in `web/operations.rs`. `write` creates the artifact if absent,
// with authorship derived from the credential (`user` for a human, `agent`
// for a session) rather than the body. Thread operations duplicate the small
// mapping/resolution helpers in `web/discussion.rs` instead of reaching into
// that sibling module's private items; the domain calls
// (`weaver_core::discussion`) are the same.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<list::Op, _, _>(list_operation),
        register::<get::Op, _, _>(get_operation),
        register::<write::Op, _, _>(write_operation),
        register::<delete::Op, _, _>(delete_operation),
        register::<history::Op, _, _>(history_operation),
        register::<threads::list::Op, _, _>(threads_list_operation),
        register::<threads::comment::Op, _, _>(threads_comment_operation),
        register::<threads::resolve::Op, _, _>(threads_resolve_operation),
        register::<url::Op, _, _>(url_operation),
    ]
}

async fn list_operation(context: OperationContext, input: list::Input) -> ApiResult<list::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let artifacts = if input.repo {
        artifact::list_for_repo(&st.db, &branch.repo_root).await?
    } else {
        artifact::list_for_session(&st.db, &branch.repo_root, &branch.id).await?
    };
    Ok(artifacts.iter().map(artifact_meta).collect())
}

/// `artifacts.get`. Returns the envelope, content, version list, and resolved
/// reference map (issue chips) via [`artifact_view`].
async fn get_operation(context: OperationContext, input: get::Input) -> ApiResult<get::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let a = if input.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &input.name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &input.name).await?
    }
    .ok_or_else(|| AppError::not_found("artifact"))?;
    artifact_view(&st.db, &branch.repo_root, &a, input.rev).await
}

/// `artifacts.write` — unified operation for session-scoped and branch-scoped writes.
///
/// `content` is an ordinary string field in the request body (see `write.rs`);
/// the `#[operand(from_file)]` attribute is a CLI-only convenience over the
/// same JSON body and has no effect here.
async fn write_operation(
    context: OperationContext,
    input: write::Input,
) -> ApiResult<write::Output> {
    let st = context.state.clone();
    let branch = require_branch(&st.db, &input.branch).await?;
    write_for_branch(context, input, branch).await
}

async fn write_for_branch(
    context: OperationContext,
    input: write::Input,
    branch: Branch,
) -> ApiResult<write::Output> {
    let st = context.state.clone();
    let author = if context.principal.is_human() {
        "user"
    } else {
        "agent"
    };
    let a = artifact::write_resolved(
        &st.db,
        &artifact::WriteRevision {
            repo_root: &branch.repo_root,
            scope: if input.repo {
                artifact::WriteScope::Shared
            } else {
                artifact::WriteScope::Branch(&branch.id)
            },
            name: &input.name,
            kind: input.kind.as_deref(),
            title: input.title.as_deref(),
            content: &input.content,
            author,
            base_rev: input.base_rev,
        },
    )
    .await
    .map_err(
        |error| match error.downcast_ref::<artifact::RevisionConflict>() {
            Some(conflict) => {
                AppError::conflict("stale").with_fields(json!({ "latest": conflict.latest }))
            }
            None => AppError::from(error),
        },
    )?;
    // `goal` is the canonical goal artifact — keep the denormalized
    // `branches.goal` cache column in sync with what was just written.
    if a.name == "goal" {
        branch_mod::sync_goal_cache(&st.db, &branch.id).await?;
    }
    if a.rev == 1 {
        tracing::info!(artifact = %a.name, rev = a.rev, "artifact created");
    } else {
        tracing::info!(artifact = %a.name, rev = a.rev, "artifact updated");
    }
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_written",
        json!({ "name": a.name, "rev": a.rev, "title": a.title }),
    )
    .await
    .ok();
    // A wired thread's status card links the session's documents — refresh it so
    // a new doc appears there without waiting for a status write.
    crate::slack::spawn_status_mirrors(st.clone(), branch.id.clone());
    artifact_view(&st.db, &branch.repo_root, &a, Some(a.rev)).await
}

async fn delete_operation(
    context: OperationContext,
    input: delete::Input,
) -> ApiResult<delete::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let a = if input.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &input.name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &input.name).await?
    }
    .ok_or_else(|| AppError::not_found("artifact"))?;
    discussion::delete_for_artifact(&st.db, a.id).await?;
    artifact::delete(&st.db, a.id).await?;
    tracing::info!(artifact = %a.name, "artifact deleted");
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_deleted",
        json!({ "name": a.name, "branch_id": a.branch_id }),
    )
    .await
    .ok();
    // A wired thread's status card lists the session's documents — refresh it so
    // a deleted doc stops appearing there.
    crate::slack::spawn_status_mirrors(st.clone(), branch.id.clone());
    Ok(weaver_api::ArtifactDeleteResult {
        deleted: true,
        name: a.name,
    })
}

/// `artifacts.history` — standalone version list query for a caller that wants
/// versions without re-fetching the latest content.
async fn history_operation(
    context: OperationContext,
    input: history::Input,
) -> ApiResult<history::Output> {
    let st = context.state;
    let branch = require_branch(&st.db, &input.branch).await?;
    let a = if input.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &input.name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &input.name).await?
    }
    .ok_or_else(|| AppError::not_found("artifact"))?;
    let versions = artifact::history(&st.db, a.id)
        .await?
        .into_iter()
        .map(|v| ArtifactVersion {
            rev: v.rev,
            author: v.author,
            created_at: v.created_at,
        })
        .collect();
    Ok(versions)
}

/// Map a domain [`discussion::Thread`] to its [`ThreadDto`] response.
/// Duplicated from the private `thread_dto` in `web/discussion.rs`, which
/// isn't visible to a sibling module.
fn thread_dto(t: &discussion::Thread) -> ThreadDto {
    ThreadDto {
        id: t.id,
        base_rev: t.base_rev,
        anchor: AnchorDto {
            quote: t.anchor_quote.clone(),
            prefix: t.anchor_prefix.clone(),
            suffix: t.anchor_suffix.clone(),
        },
        status: t.status.clone(),
        created_at: t.created_at.clone(),
        resolved_at: t.resolved_at.clone(),
        comments: t
            .comments
            .iter()
            .map(|c| CommentDto {
                seq: c.seq,
                author: c.author.clone(),
                body: c.body.clone(),
                created_at: c.created_at.clone(),
            })
            .collect(),
    }
}

/// Resolve `{branch, name}` to the artifact a branch-scoped thread operation
/// targets — branch-scoped first, then repo-shared, the same resolution
/// `artifacts.get` does.
async fn thread_artifact(
    st: &AppState,
    branch_key: &str,
    name: &str,
) -> ApiResult<(Branch, Artifact)> {
    let branch = require_branch(&st.db, branch_key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    Ok((branch, a))
}

/// `artifacts.threads.list`. The `open_only` flag (inverse of `all`) defaults
/// to true: listing open threads only unless the caller requests all statuses.
async fn threads_list_operation(
    context: OperationContext,
    input: threads::list::Input,
) -> ApiResult<threads::list::Output> {
    let st = context.state;
    let (_, a) = thread_artifact(&st, &input.branch, &input.name).await?;
    // `open_only` narrows; the default lists resolved threads too.
    let threads = discussion::list_for_artifact(&st.db, a.id, !input.open_only).await?;
    Ok(threads.iter().map(thread_dto).collect())
}

/// `artifacts.threads.comment` — open a thread (`New`) or reply to one
/// (`Reply`) in a single operation. Author is always `"agent"`: this is a
/// session-actor, MCP-reachable operation, not the dashboard's own edits.
/// Output is the full [`ThreadDto`] for both targets, so the reply path
/// re-fetches the thread after appending, matching what the new-thread path
/// already returns.
async fn threads_comment_operation(
    context: OperationContext,
    input: threads::comment::Input,
) -> ApiResult<threads::comment::Output> {
    let st = context.state;
    let (branch, a) = thread_artifact(&st, &input.branch, &input.name).await?;
    let thread = match input.target {
        threads::comment::CommentTarget::New { base_rev, anchor } => {
            let thread = discussion::create_thread(
                &st.db,
                &discussion::NewThread {
                    artifact_id: a.id,
                    base_rev,
                    anchor_quote: &anchor.quote,
                    anchor_prefix: &anchor.prefix,
                    anchor_suffix: &anchor.suffix,
                    author: "agent",
                    body: &input.body,
                },
            )
            .await?;
            tracing::info!(artifact = %input.name, thread = thread.id, "comment posted");
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "comment_added",
                json!({ "artifact": input.name, "thread": thread.id, "seq": 1, "author": "agent" }),
            )
            .await
            .ok();
            thread
        }
        threads::comment::CommentTarget::Reply { thread_id } => {
            let thread = discussion::get_thread(&st.db, thread_id)
                .await?
                .filter(|t| t.artifact_id == a.id)
                .ok_or_else(|| AppError::not_found("thread"))?;
            let comment = discussion::add_comment(&st.db, thread.id, "agent", &input.body).await?;
            tracing::info!(artifact = %input.name, thread = thread.id, seq = comment.seq, "comment posted");
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "comment_added",
                json!({ "artifact": input.name, "thread": thread.id, "seq": comment.seq, "author": "agent" }),
            )
            .await
            .ok();
            discussion::get_thread(&st.db, thread.id)
                .await?
                .ok_or_else(|| AppError::not_found("thread"))?
        }
    };
    Ok(thread_dto(&thread))
}

/// `artifacts.threads.resolve`. Output is the full [`ThreadDto`], so this
/// re-fetches the thread after resolving rather than returning a bare
/// acknowledgement.
async fn threads_resolve_operation(
    context: OperationContext,
    input: threads::resolve::Input,
) -> ApiResult<threads::resolve::Output> {
    let st = context.state;
    let (branch, a) = thread_artifact(&st, &input.branch, &input.name).await?;
    let thread = discussion::get_thread(&st.db, input.thread_id)
        .await?
        .filter(|t| t.artifact_id == a.id)
        .ok_or_else(|| AppError::not_found("thread"))?;
    discussion::resolve(&st.db, thread.id).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "comment_resolved",
        json!({ "artifact": input.name, "thread": thread.id }),
    )
    .await
    .ok();
    let resolved = discussion::get_thread(&st.db, thread.id)
        .await?
        .ok_or_else(|| AppError::not_found("thread"))?;
    Ok(thread_dto(&resolved))
}

/// `artifacts.url`. The dispatcher hands handlers typed input rather than a
/// raw request, so — same as `sessions.url` — this resolves the URL from the
/// configured `auth.base_url` or the address the server is bound to; it has
/// no way to read a browser's own Host header.
async fn url_operation(context: OperationContext, input: url::Input) -> ApiResult<SessionUrlView> {
    let st = context.state;
    // Resolve first so a bad key 404s rather than minting a link to nothing,
    // and so the link always keys off the canonical branch id even when the
    // caller named the branch some other way `require_branch` also accepts.
    let branch = require_branch(&st.db, &input.branch).await?;
    let base = super::auth::public_base(&st, &header::HeaderMap::new()).await;
    Ok(SessionUrlView {
        url: crate::links::artifact_url(&base, &branch.id, &input.name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthVia, Grant, Principal};
    use crate::db::Db;

    /// A session credential, for authorship-derivation tests.
    fn session_context(state: AppState, branch_id: &str) -> OperationContext {
        OperationContext::new(
            state,
            Principal {
                username: "agent".to_string(),
                github_login: None,
                via: AuthVia::Token,
                grant: Grant::Session {
                    session_id: "session".to_string(),
                    branch_id: branch_id.to_string(),
                    capabilities: None,
                },
                automation_context: None,
            },
        )
    }

    fn test_state(db: Db) -> AppState {
        AppState {
            ctx: crate::Ctx {
                db: db.clone(),
                bus: crate::events::EventBus::new(),
                addr: "127.0.0.1:0".to_string(),
            },
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        }
    }

    #[test]
    fn decodes_typed_and_legacy_image_artifacts() {
        let png = "data:image/png;base64,aGVsbG8=";
        let (mime, bytes) = decode_artifact_image(png).expect("typed image");
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"hello");

        let legacy = format!("![Screenshot]({png})\n");
        let (mime, bytes) = decode_artifact_image(&legacy).expect("legacy image wrapper");
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn rejects_non_image_and_non_standalone_artifacts() {
        assert!(decode_artifact_image("# Notes\n\nNot an image.").is_none());
        assert!(
            decode_artifact_image("Before\n\n![Screenshot](data:image/png;base64,aGVsbG8=)")
                .is_none()
        );
        assert!(decode_artifact_image("data:text/html;base64,aGVsbG8=").is_none());
        assert!(decode_artifact_image("data:image/png;base64,not!base64").is_none());
    }

    #[tokio::test]
    async fn write_creates_then_appends_a_guarded_revision() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();

        let view = write_operation(
            session_context(st.clone(), &branch.id),
            write::Input {
                name: "plan".to_string(),
                content: "v1".to_string(),
                title: Some("The Plan".to_string()),
                kind: None,
                base_rev: None,
                repo: false,
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(view.content, "v1");
        assert_eq!(view.meta.rev, 1);
        assert_eq!(view.meta.branch_id.as_deref(), Some(branch.id.as_str()));

        // Appends a revision; still attributed to `agent` (a session credential).
        let view = write_operation(
            session_context(st.clone(), &branch.id),
            write::Input {
                name: "plan".to_string(),
                content: "v2".to_string(),
                title: None,
                kind: None,
                base_rev: Some(1),
                repo: false,
                branch: branch.id.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(view.content, "v2");
        assert_eq!(view.meta.rev, 2);
        assert_eq!(view.meta.title, "The Plan", "title carries over unset");
        assert_eq!(view.versions[0].author, "agent");

        let stale = write_operation(
            session_context(st, &branch.id),
            write::Input {
                name: "plan".to_string(),
                content: "stale edit".to_string(),
                title: None,
                kind: None,
                base_rev: Some(1),
                repo: false,
                branch: branch.id,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(stale.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(stale.fields.unwrap()["latest"], 2);
    }
    fn edit(branch: &Branch, name: &str, content: &str, base_rev: Option<i64>) -> write::Input {
        write::Input {
            name: name.to_string(),
            content: content.to_string(),
            title: None,
            kind: None,
            base_rev,
            repo: false,
            branch: branch.id.clone(),
        }
    }

    // Both requests enter the real operation's mutation phase while a writer
    // holds the permit. Polling queues them in FIFO order without sleeps or
    // scheduler timing; only branch-key validation has already completed.
    async fn race(
        st: &AppState,
        branch: &Branch,
        first: write::Input,
        second: write::Input,
    ) -> (ApiResult<ArtifactView>, ApiResult<ArtifactView>) {
        let writer = weaver_core::db::begin_immediate(&st.db).await.unwrap();
        let mut first = Box::pin(write_for_branch(
            session_context(st.clone(), &branch.id),
            first,
            branch.clone(),
        ));
        let mut second = Box::pin(write_for_branch(
            session_context(st.clone(), &branch.id),
            second,
            branch.clone(),
        ));
        assert!(futures_util::poll!(&mut first).is_pending());
        assert!(futures_util::poll!(&mut second).is_pending());
        writer.commit().await.unwrap();
        tokio::join!(first, second)
    }

    async fn assert_conflict(error: AppError, latest: i64) {
        let response = error.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({"error": "stale", "latest": latest}));
    }

    async fn fixture() -> (AppState, Branch) {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();
        (test_state(db), branch)
    }

    #[tokio::test]
    async fn atomic_claim_rejects_concurrent_reorder_and_promotion() {
        for stale in ["pending=[B,A]; active=null", "pending=[A]; active=B"] {
            let (st, branch) = fixture().await;
            let mut seed = edit(
                &branch,
                "change-queue.json",
                "pending=[A,B]; active=null",
                Some(0),
            );
            seed.title = Some("Changes".into());
            seed.kind = Some("json".into());
            write_operation(session_context(st.clone(), &branch.id), seed)
                .await
                .unwrap();
            let mut loser = edit(&branch, "change-queue.json", stale, Some(1));
            loser.title = Some("stale title".into());
            loser.kind = Some("markdown".into());
            let (claim, rejected) = race(
                &st,
                &branch,
                edit(
                    &branch,
                    "change-queue.json",
                    "pending=[B]; active=A",
                    Some(1),
                ),
                loser,
            )
            .await;
            let claim = claim.unwrap();
            assert_eq!(claim.meta.rev, 2);
            assert_eq!(claim.content, "pending=[B]; active=A");
            assert_eq!(claim.meta.title, "Changes");
            assert_eq!(claim.meta.kind, "json");
            assert_conflict(rejected.unwrap_err(), 2).await;
            let current = artifact::get_by_id(&st.db, claim.meta.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(artifact_meta(&current)).unwrap(),
                serde_json::to_value(&claim.meta).unwrap()
            );
            assert_eq!(
                artifact::history(&st.db, current.id).await.unwrap().len(),
                2
            );
            let events: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'artifact_written'")
                    .fetch_one(&st.db)
                    .await
                    .unwrap();
            assert_eq!(events, 2, "the losing request emits no event");
        }
    }

    #[tokio::test]
    async fn atomic_two_promotions_have_one_winner() {
        let (st, branch) = fixture().await;
        write_operation(
            session_context(st.clone(), &branch.id),
            edit(&branch, "queue", "pending=[A,B]; active=null", Some(0)),
        )
        .await
        .unwrap();
        let (first, second) = race(
            &st,
            &branch,
            edit(&branch, "queue", "pending=[B]; active=A", Some(1)),
            edit(&branch, "queue", "pending=[A]; active=B", Some(1)),
        )
        .await;
        assert_eq!(first.unwrap().content, "pending=[B]; active=A");
        assert_conflict(second.unwrap_err(), 2).await;
    }

    #[tokio::test]
    async fn atomic_creation_allocates_one_identity_and_version() {
        let (st, branch) = fixture().await;
        let before: i64 = sqlx::query_scalar("SELECT next_id FROM artifact_id_sequence")
            .fetch_one(&st.db)
            .await
            .unwrap();
        let (first, second) = race(
            &st,
            &branch,
            edit(&branch, "goal", "winner", Some(0)),
            edit(&branch, "goal", "loser", Some(0)),
        )
        .await;
        let first = first.unwrap();
        assert_eq!(first.meta.id, before);
        assert_eq!(first.meta.rev, 1);
        assert_conflict(second.unwrap_err(), 1).await;
        let after: i64 = sqlx::query_scalar("SELECT next_id FROM artifact_id_sequence")
            .fetch_one(&st.db)
            .await
            .unwrap();
        assert_eq!(after, before + 1);
        assert_eq!(
            artifact::list_for_repo(&st.db, "/r").await.unwrap().len(),
            1
        );
        assert_eq!(
            artifact::history(&st.db, first.meta.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            branch_mod::get(&st.db, &branch.id)
                .await
                .unwrap()
                .unwrap()
                .goal,
            "winner"
        );
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'artifact_written'")
                .fetch_one(&st.db)
                .await
                .unwrap();
        assert_eq!(events, 1);
    }

    #[tokio::test]
    async fn atomic_branch_view_resolves_shared_inside_the_transaction() {
        let (st, branch) = fixture().await;
        let mut shared = edit(&branch, "queue", "shared", Some(0));
        shared.repo = true;
        // The shared artifact does not exist until the first queued request
        // commits. The branch-view request must then resolve that identity.
        let (created, rejected) = race(
            &st,
            &branch,
            shared,
            edit(&branch, "queue", "branch stale creation", Some(0)),
        )
        .await;
        let created = created.unwrap();
        assert_eq!(created.meta.branch_id, None);
        assert_conflict(rejected.unwrap_err(), 1).await;
        let updated = write_operation(
            session_context(st.clone(), &branch.id),
            edit(&branch, "queue", "shared update", Some(1)),
        )
        .await
        .unwrap();
        assert_eq!(updated.meta.id, created.meta.id);
        assert_eq!(updated.meta.branch_id, None);

        // A shadow is created after the branch edit has been queued. Identity
        // and omitted metadata must resolve to the new branch envelope under
        // the writer lock, rather than the formerly visible shared envelope.
        let writer = weaver_core::db::begin_immediate(&st.db).await.unwrap();
        let new = artifact::NewRevision {
            repo_root: "/r",
            branch_id: Some(&branch.id),
            name: "queue",
            kind: "json",
            title: "Branch queue",
            content: "branch",
            author: "agent",
        };
        let mut shadow = Box::pin(artifact::write(&st.db, &new));
        let mut branch_edit = Box::pin(write_for_branch(
            session_context(st.clone(), &branch.id),
            edit(&branch, "queue", "new shadow update", Some(1)),
            branch.clone(),
        ));
        assert!(futures_util::poll!(&mut shadow).is_pending());
        assert!(futures_util::poll!(&mut branch_edit).is_pending());
        writer.commit().await.unwrap();
        let (shadow, branch_edit) = tokio::join!(shadow, branch_edit);
        let shadow = shadow.unwrap();
        let branch_edit = branch_edit.unwrap();
        assert_eq!(branch_edit.meta.id, shadow.id);
        assert_eq!(branch_edit.meta.rev, 2);
        assert_eq!(branch_edit.meta.title, "Branch queue");
        assert_eq!(branch_edit.meta.kind, "json");
        let mut shared_edit = edit(&branch, "queue", "shared independent", Some(2));
        shared_edit.repo = true;
        let (local, shared) = race(
            &st,
            &branch,
            edit(&branch, "queue", "branch update", Some(2)),
            shared_edit,
        )
        .await;
        let local = local.unwrap();
        let shared = shared.unwrap();
        assert_eq!(local.meta.id, shadow.id);
        assert_eq!(local.meta.title, "Branch queue");
        assert_eq!(local.meta.kind, "json");
        assert_eq!(local.content, "branch update");
        assert_eq!(shared.meta.id, created.meta.id);
        assert_eq!(shared.content, "shared independent");
    }

    #[tokio::test]
    async fn stale_write_changes_no_metadata_sequence_goal_cache_or_events() {
        let (st, branch) = fixture().await;
        write_operation(
            session_context(st.clone(), &branch.id),
            edit(&branch, "goal", "current", Some(0)),
        )
        .await
        .unwrap();
        async fn snapshot(db: &Db) -> Vec<Vec<String>> {
            let mut rows = Vec::new();
            for table in [
                "artifacts",
                "artifact_versions",
                "artifact_id_sequence",
                "branches",
                "events",
            ] {
                // SQLite's JSON export captures every column, including all
                // timestamps, rather than checking only the latest content.
                let columns: Vec<String> =
                    sqlx::query_scalar("SELECT name FROM pragma_table_info(?)")
                        .bind(table)
                        .fetch_all(db)
                        .await
                        .unwrap();
                let fields = columns
                    .iter()
                    .map(|c| format!("'{c}', {c}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let values: Vec<String> = sqlx::query_scalar(&format!(
                    "SELECT json_object({fields}) FROM {table} ORDER BY rowid"
                ))
                .fetch_all(db)
                .await
                .unwrap();
                rows.push(values);
            }
            rows
        }
        let before = snapshot(&st.db).await;
        let mut stale = edit(&branch, "goal", "stale", Some(0));
        stale.title = Some("stale title".into());
        stale.kind = Some("image".into());
        assert_conflict(
            write_operation(session_context(st.clone(), &branch.id), stale)
                .await
                .unwrap_err(),
            1,
        )
        .await;
        assert_eq!(snapshot(&st.db).await, before);
        assert_conflict(
            write_operation(
                session_context(st.clone(), &branch.id),
                edit(&branch, "absent", "stale", Some(5)),
            )
            .await
            .unwrap_err(),
            0,
        )
        .await;
        assert_eq!(snapshot(&st.db).await, before);
    }

    #[tokio::test]
    async fn unguarded_responses_keep_their_own_revision_and_content() {
        let (st, branch) = fixture().await;
        let mut first_edit = edit(&branch, "queue", "first", None);
        first_edit.title = Some("First title".into());
        first_edit.kind = Some("json".into());
        let (first, second) = race(
            &st,
            &branch,
            first_edit,
            edit(&branch, "queue", "second", None),
        )
        .await;
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!((first.meta.rev, first.content.as_str()), (1, "first"));
        assert_eq!((second.meta.rev, second.content.as_str()), (2, "second"));
        assert_eq!(second.meta.title, "First title");
        assert_eq!(second.meta.kind, "json");
        // Explicitly reconstruct the first response after the second commit,
        // using the same revision-pinned view path as the write operation.
        let envelope = Artifact {
            id: first.meta.id,
            repo_root: "/r".into(),
            branch_id: first.meta.branch_id,
            name: first.meta.name,
            kind: first.meta.kind,
            title: first.meta.title,
            rev: first.meta.rev,
            created_at: first.meta.created_at,
            updated_at: first.meta.updated_at,
        };
        let late = artifact_view(&st.db, "/r", &envelope, Some(envelope.rev))
            .await
            .unwrap();
        assert_eq!((late.meta.rev, late.content.as_str()), (1, "first"));
    }

    #[tokio::test]
    async fn openapi_advertises_the_atomic_write_behavior_tested_here() {
        let (st, branch) = fixture().await;
        crate::auth::add_user(
            &st.db,
            "atomic-test",
            None,
            None,
            None,
            crate::auth::UserRole::Admin,
        )
        .await
        .unwrap();
        let (token, _) = crate::auth::create_token(&st.db, "atomic-test", "test", None)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                super::super::router(st)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let response = client
            .get(format!("http://{address}/api/openapi.json"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let document: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            document["paths"]["/api/artifacts/write"]["post"]["x-loom-atomic-base-rev"],
            json!(true)
        );
        assert!(document["paths"]["/api/artifacts/get"]["post"]
            .get("x-loom-atomic-base-rev")
            .is_none());
        let url = format!("http://{address}/api/artifacts/write");
        let input = edit(&branch, "queue", "winner", Some(0));
        let created = client
            .post(&url)
            .bearer_auth(&token)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), reqwest::StatusCode::OK);
        let created: ArtifactView = created.json().await.unwrap();
        assert_eq!((created.meta.rev, created.content.as_str()), (1, "winner"));
        let rejected = client
            .post(&url)
            .bearer_auth(&token)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(
            rejected.json::<serde_json::Value>().await.unwrap(),
            json!({"error": "stale", "latest": 1})
        );
        server.abort();
    }
}
