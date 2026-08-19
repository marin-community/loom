use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::operations::artifacts::{delete, get, history, list, threads, url, write};
use weaver_api::{
    AnchorDto, ArtifactMeta, ArtifactRefs, ArtifactUpsertReq, ArtifactVersion, ArtifactView,
    ArtifactWriteBody, CommentDto, IssueRefStatus, SessionUrlView, ThreadDto,
};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch as branch_mod;
use weaver_core::branch::Branch;
use weaver_core::discussion;

use crate::db::Db;
use crate::events;

use super::operations::{register, Bound, OperationContext};
use super::{require_branch, require_session};
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Artifacts — named, versioned documents stored in weaver.db. The GET resolves
// the content's references against the issue ledger (via smartdoc) and returns
// the projection alongside, so the SPA chips and `loom artifacts get` render
// the same join. Structure in the doc, state in the DB. See docs/artifacts.md.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct RevQuery {
    rev: Option<i64>,
}

const MAX_ARTIFACT_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Pull a standalone image data URI out of artifact content. New writes store
/// the URI directly under the explicit `image` kind; the markdown case
/// preserves artifacts written before that kind existed. Content detection
/// also keeps historical revisions readable if an artifact's envelope kind
/// changes later (kind is current metadata, not versioned metadata).
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

/// The wire metadata for an artifact envelope.
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

/// List the artifacts visible from a session: its branch's plus the repo-shared
/// ones, latest rev each (a branch-scoped name shadows a shared one).
pub(super) async fn list_artifacts(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<ArtifactMeta>>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let artifacts = artifact::list_for_session(&st.db, &branch.repo_root, &branch.id).await?;
    Ok(Json(artifacts.iter().map(artifact_meta).collect()))
}

/// Resolve an artifact's content references to their live status, as the wire
/// [`ArtifactRefs`]. Probes each `#N` against the repo's issue ledger and joins
/// via [`smartdoc::project`]; an unresolved reference is omitted from the map.
async fn project_artifact_refs(db: &Db, repo_root: &str, content: &str) -> ArtifactRefs {
    let doc = smartdoc::parse(content);
    // Probe each distinct reference against weaver-core. Best-effort: a probe
    // miss (unknown issue, wrong repo, read error) just leaves that ref absent
    // from the status map, which `project` renders as a muted, non-existent chip.
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
    // Join, then shape the resolved issue refs into the wire map (keyed by id).
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
/// latest): envelope, content, version list, and the projected reference map.
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

/// One artifact, content + projected refs, resolving branch-scoped before
/// repo-shared. `?rev=N` selects a revision; the default is latest.
pub(super) async fn get_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Query(q): Query<RevQuery>,
) -> ApiResult<Json<ArtifactView>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, q.rev).await?,
    ))
}

/// Raw bytes for a standalone image artifact. Markdown documents can use
/// `![alt](artifact:<name>)`; the renderer maps that source to this route, so
/// the browser depends only on loom's artifact store (never an agent-local
/// path). `?rev=N` pins an older image revision; omitted means latest.
pub(super) async fn raw_artifact_image(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Query(q): Query<RevQuery>,
) -> ApiResult<Response> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    let version = match q.rev {
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

/// Write a new revision of an artifact (a user edit, `author: user`), returning
/// the refreshed view at the new latest revision. The artifact must already
/// exist in the session's view; the write targets the resolved scope (its own
/// branch-scoped row, else the repo-shared one).
pub(super) async fn write_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Json(body): Json<ArtifactWriteBody>,
) -> ApiResult<Json<ArtifactView>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let existing = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    // Optimistic-concurrency guard: a caller that read a specific revision
    // and supplies `base_rev` gets rejected if someone else has written since
    // — rather than silently clobbering that newer revision. Omitting
    // `base_rev` force-writes, same as before this guard existed.
    if let Some(b) = body.base_rev {
        if b != existing.rev {
            return Err(AppError::conflict("stale").with_fields(json!({ "latest": existing.rev })));
        }
    }
    // Keep the existing kind/title unless the body overrides them.
    let kind = body.kind.unwrap_or_else(|| existing.kind.clone());
    let title = body.title.unwrap_or_else(|| existing.title.clone());
    // Write into the same scope the artifact resolved to (a shared artifact
    // edited from a session writes a new shared revision, not a branch copy).
    let scope = existing.branch_id.as_deref();
    let a = artifact::write(
        &st.db,
        &artifact::NewRevision {
            repo_root: &branch.repo_root,
            branch_id: scope,
            name: &name,
            kind: &kind,
            title: &title,
            content: &body.content,
            author: "user",
        },
    )
    .await?;
    // `goal` is the canonical goal artifact — keep the denormalized
    // `branches.goal` cache column in sync with what was just written.
    if a.name == "goal" {
        branch_mod::sync_goal_cache(&st.db, &branch.id).await?;
    }
    tracing::info!(artifact = %a.name, rev = a.rev, "artifact updated");
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
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, None).await?,
    ))
}

/// Delete an artifact and its whole revision history. Resolves the name the way
/// the session sees it (its own branch-scoped row, else the repo-shared one — the
/// single row the list shows for that name), so deleting from the UI removes
/// exactly the artifact displayed. Broadcasts `artifact_deleted` for live refresh.
pub(super) async fn delete_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (_, branch) = require_session(&st.db, &key).await?;
    let a = artifact::get(&st.db, &branch.repo_root, &branch.id, &name)
        .await?
        .ok_or_else(|| AppError::not_found("artifact"))?;
    // FKs are off on the pool, so `ON DELETE CASCADE` doesn't fire — clean up
    // the artifact's discussion threads/comments explicitly before/with it.
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
    Ok(Json(json!({ "deleted": true, "name": a.name })))
}

// ---------------------------------------------------------------------------
// Branch-scoped artifacts — the twin of the session-scoped routes above, for
// a `loom artifacts` target with no live session. `PUT` here creates the
// artifact if absent (the session-scoped `PUT` requires it to already exist,
// since that route is a *user edit* of something the dashboard is already
// showing); `author` defaults to `agent`, the CLI's writer.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub(super) struct ArtifactScopeQuery {
    #[serde(default)]
    repo: bool,
}

pub(super) async fn list_branch_artifacts(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<ArtifactScopeQuery>,
) -> ApiResult<Json<Vec<ArtifactMeta>>> {
    let branch = require_branch(&st.db, &key).await?;
    let artifacts = if q.repo {
        artifact::list_for_repo(&st.db, &branch.repo_root).await?
    } else {
        artifact::list_for_session(&st.db, &branch.repo_root, &branch.id).await?
    };
    Ok(Json(artifacts.iter().map(artifact_meta).collect()))
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ArtifactGetQuery {
    rev: Option<i64>,
    #[serde(default)]
    repo: bool,
}

pub(super) async fn get_branch_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Query(q): Query<ArtifactGetQuery>,
) -> ApiResult<Json<ArtifactView>> {
    let branch = require_branch(&st.db, &key).await?;
    let a = if q.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &name).await?
    }
    .ok_or_else(|| AppError::not_found("artifact"))?;
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, q.rev).await?,
    ))
}

pub(super) async fn write_branch_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Json(body): Json<ArtifactUpsertReq>,
) -> ApiResult<Json<ArtifactView>> {
    let branch = require_branch(&st.db, &key).await?;
    let existing = if body.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &name).await?
    };
    if let Some(base_rev) = body.base_rev {
        let latest = existing.as_ref().map_or(0, |artifact| artifact.rev);
        if base_rev != latest {
            return Err(AppError::conflict("stale").with_fields(json!({ "latest": latest })));
        }
    }
    let kind = body
        .kind
        .clone()
        .or_else(|| existing.as_ref().map(|a| a.kind.clone()))
        .unwrap_or_else(|| "markdown".to_string());
    let title = body
        .title
        .clone()
        .or_else(|| existing.as_ref().map(|a| a.title.clone()))
        .unwrap_or_default();
    let author = body.author.as_deref().unwrap_or("agent").to_string();
    // `repo: true` writes the repo-shared scope explicitly; otherwise write
    // into whatever scope the name already resolved to (a shared artifact
    // edited from a branch writes a new shared revision, not a branch copy),
    // defaulting to this branch's own scope for a brand-new name.
    let scope: Option<String> = if body.repo {
        None
    } else {
        existing
            .as_ref()
            .and_then(|a| a.branch_id.clone())
            .or_else(|| Some(branch.id.clone()))
    };
    let a = artifact::write(
        &st.db,
        &artifact::NewRevision {
            repo_root: &branch.repo_root,
            branch_id: scope.as_deref(),
            name: &name,
            kind: &kind,
            title: &title,
            content: &body.content,
            author: &author,
        },
    )
    .await?;
    // `goal` is the canonical goal artifact — keep the denormalized
    // `branches.goal` cache column in sync with what was just written.
    if a.name == "goal" {
        branch_mod::sync_goal_cache(&st.db, &branch.id).await?;
    }
    if existing.is_none() {
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
    // Same card refresh as the session-scoped write route above.
    crate::slack::spawn_status_mirrors(st.clone(), branch.id.clone());
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, None).await?,
    ))
}

/// `GET /api/branches/{key}/artifacts/{name}/url` — the dashboard deep-link for
/// an artifact.
///
/// The twin of `session_url_route`: the agent that just wrote the artifact holds
/// only the loopback (or wildcard) `$WEAVER_API` it was handed, and a
/// `http://0.0.0.0:7878/…` link printed after a write is useless to whoever
/// reads it. Only the server knows the externally-visible origin (the operator's
/// `auth.base_url`, else the request's own Host), so resolving it is the
/// server's job — see `loom artifacts write`.
pub(super) async fn branch_artifact_url_route(
    State(st): State<AppState>,
    headers: header::HeaderMap,
    Path((key, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    // Resolve the branch so a bad key 404s rather than minting a link to
    // nothing; the URL itself keys off the caller's `key` (the `$WEAVER_BRANCH`
    // the SPA router resolves), exactly as the write output always has.
    require_branch(&st.db, &key).await?;
    let base = super::auth::public_base(&st, &headers).await;
    Ok(Json(
        json!({ "url": crate::links::artifact_url(&base, &key, &name) }),
    ))
}

pub(super) async fn delete_branch_artifact(
    State(st): State<AppState>,
    Path((key, name)): Path<(String, String)>,
    Query(q): Query<ArtifactScopeQuery>,
) -> ApiResult<Json<Value>> {
    let branch = require_branch(&st.db, &key).await?;
    let a = if q.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &name).await?
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
    Ok(Json(json!({ "deleted": true, "name": a.name })))
}

// ---------------------------------------------------------------------------
// Operation registry — `artifacts.*` and `artifacts.threads.*`, bound onto
// `weaver_api::operations::artifacts`. These are the branch-scoped twins
// above, ported: an operation's `branch` field is resolved the same way `key`
// is on the routes (branch id, branch name, or an active session's branch),
// and authorization now happens once, centrally, in `web/operations.rs`. The
// legacy routes above stay live and untouched until the coordinated route
// deletion pass. Thread operations duplicate the small mapping/resolution
// helpers in `web/discussion.rs` rather than reach into that sibling module's
// private items; the domain calls (`weaver_core::discussion`) are the same.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<list::List, _, _>(list_operation),
        register::<get::Get, _, _>(get_operation),
        register::<write::Write, _, _>(write_operation),
        register::<delete::Delete, _, _>(delete_operation),
        register::<history::History, _, _>(history_operation),
        register::<threads::list::List, _, _>(threads_list_operation),
        register::<threads::comment::Comment, _, _>(threads_comment_operation),
        register::<threads::resolve::Resolve, _, _>(threads_resolve_operation),
        register::<url::Url, _, _>(url_operation),
    ]
}

/// `artifacts.list` — the twin of [`list_branch_artifacts`].
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

/// `artifacts.get` — the twin of [`get_branch_artifact`]. Reuses
/// [`artifact_view`], so the projected reference map (issue chips) rides
/// along exactly as it does for the legacy route.
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

/// `artifacts.write` — one operation replacing the session-scoped and
/// branch-scoped routes that used to differ in who they said wrote a revision.
///
/// `content` is an ordinary string field on the wire (see `write.rs`); the
/// `#[operand(from_file)]` attribute is a CLI-only convenience over the same
/// JSON body and has no effect here.
///
/// Authorship is derived from the credential, never from the body. The two old
/// routes disagreed: the session-scoped one hard-coded `"user"`, while the
/// branch-scoped one took `author` from the request, so any caller could claim
/// to be anyone. Reading it off the principal makes the attribution true by
/// construction and removes a field a caller could lie in.
async fn write_operation(
    context: OperationContext,
    input: write::Input,
) -> ApiResult<write::Output> {
    let st = context.state.clone();
    let branch = require_branch(&st.db, &input.branch).await?;
    let existing = if input.repo {
        artifact::get_shared(&st.db, &branch.repo_root, &input.name).await?
    } else {
        artifact::get(&st.db, &branch.repo_root, &branch.id, &input.name).await?
    };
    // Optimistic-concurrency guard, same as the legacy branch-scoped route:
    // this is business state ("this write raced another"), not authority, so
    // it stays here rather than moving into `authorize()`.
    if let Some(base_rev) = input.base_rev {
        let latest = existing.as_ref().map_or(0, |a| a.rev);
        if base_rev != latest {
            return Err(AppError::conflict("stale").with_fields(json!({ "latest": latest })));
        }
    }
    let title = input
        .title
        .clone()
        .or_else(|| existing.as_ref().map(|a| a.title.clone()))
        .unwrap_or_default();
    // Derived, not supplied: a human credential edits as "user", a session
    // credential as "agent".
    let author = if context.principal.is_human() {
        "user"
    } else {
        "agent"
    };
    // `repo: true` writes the repo-shared scope explicitly; otherwise write
    // into whatever scope the name already resolved to (a shared artifact
    // edited from a branch writes a new shared revision, not a branch copy),
    // defaulting to this branch's own scope for a brand-new name.
    let scope: Option<String> = if input.repo {
        None
    } else {
        existing
            .as_ref()
            .and_then(|a| a.branch_id.clone())
            .or_else(|| Some(branch.id.clone()))
    };
    // Omitting `kind` keeps whatever the artifact already is; only a new
    // artifact falls back to markdown.
    let kind = input
        .kind
        .clone()
        .or_else(|| existing.as_ref().map(|a| a.kind.clone()))
        .unwrap_or_else(|| "markdown".to_string());
    let a = artifact::write(
        &st.db,
        &artifact::NewRevision {
            repo_root: &branch.repo_root,
            branch_id: scope.as_deref(),
            name: &input.name,
            kind: &kind,
            title: &title,
            content: &input.content,
            author,
        },
    )
    .await?;
    // `goal` is the canonical goal artifact — keep the denormalized
    // `branches.goal` cache column in sync with what was just written.
    if a.name == "goal" {
        branch_mod::sync_goal_cache(&st.db, &branch.id).await?;
    }
    if existing.is_none() {
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
    artifact_view(&st.db, &branch.repo_root, &a, None).await
}

/// `artifacts.delete` — the twin of [`delete_branch_artifact`].
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

/// `artifacts.history` — no legacy route served this; the version list was
/// only ever embedded in [`ArtifactView`] (see `artifact_view`). This ports
/// the same `artifact::history` mapping standalone, for a caller that wants
/// versions without re-fetching the latest content too.
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

/// Map a domain [`discussion::Thread`] to its wire [`ThreadDto`]. Duplicated
/// from the private `thread_dto` in `web/discussion.rs` — that helper isn't
/// visible to a sibling module, and this file owns exactly the operations
/// below.
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

/// `artifacts.threads.list`, plus the `all` filter the operation's `Input`
/// declares that the route it replaced did not have (that route always listed
/// every status; `all` defaulting to `false` means the default here is
/// open-only unless the caller asks for everything).
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

/// `artifacts.threads.comment` — one operation over what used to be two
/// routes, opening a thread (`New`) and replying to one (`Reply`).
/// Author is `"agent"`, matching the branch-scoped routes it replaced (the
/// session-scoped ones hardcoded `"user"` for the dashboard's own edits,
/// which is not what this session-actor, MCP-reachable operation is). Output
/// is the full [`ThreadDto`] for both targets — `add_branch_thread_comment`
/// used to return just the new [`CommentDto`], but the declared `Output` for
/// `artifacts.threads.comment` is `ThreadDto`, so the reply path re-fetches
/// the thread after appending, the same as the new-thread path already
/// returns.
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

/// `artifacts.threads.resolve`.
/// The route it replaced returned `{"resolved": true}`; the declared `Output` for
/// `artifacts.threads.resolve` is `ThreadDto`, so this re-fetches the thread
/// after resolving and returns it in full (a superset of the old body, not a
/// dropped field).
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

/// `artifacts.url` — the twin of [`branch_artifact_url_route`]. The dispatcher
/// hands handlers typed input, not a request, so — same as `sessions.url` —
/// this can only resolve the configured `auth.base_url` or the address the
/// server is bound to, not a browser's own Host the way the REST route it
/// mirrors can.
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
    use crate::db::Db;

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
    async fn write_branch_artifact_creates_then_appends_a_revision() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();

        let view = write_branch_artifact(
            State(st.clone()),
            Path((branch.id.clone(), "plan".to_string())),
            Json(ArtifactUpsertReq {
                content: "v1".to_string(),
                title: Some("The Plan".to_string()),
                kind: None,
                author: None,
                repo: false,
                base_rev: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(view.content, "v1");
        assert_eq!(view.meta.rev, 1);
        assert_eq!(view.meta.branch_id.as_deref(), Some(branch.id.as_str()));

        // A second write with no author appends a revision, defaulting the
        // author to `agent` (the CLI's writer) — not the session route's
        // hardcoded `user`.
        let view = write_branch_artifact(
            State(st.clone()),
            Path((branch.id.clone(), "plan".to_string())),
            Json(ArtifactUpsertReq {
                content: "v2".to_string(),
                title: None,
                kind: None,
                author: None,
                repo: false,
                base_rev: Some(1),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(view.content, "v2");
        assert_eq!(view.meta.rev, 2);
        assert_eq!(view.meta.title, "The Plan", "title carries over unset");
        assert_eq!(view.versions[0].author, "agent");

        let stale = write_branch_artifact(
            State(st),
            Path((branch.id, "plan".to_string())),
            Json(ArtifactUpsertReq {
                content: "stale edit".to_string(),
                title: None,
                kind: None,
                author: None,
                repo: false,
                base_rev: Some(1),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(stale.fields.unwrap()["latest"], 2);
    }
}
