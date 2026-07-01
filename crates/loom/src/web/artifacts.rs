use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{
    ArtifactMeta, ArtifactRefs, ArtifactUpsertReq, ArtifactView, ArtifactWriteBody, IssueRefStatus,
};
use weaver_core::artifact::{self, Artifact};
use weaver_core::branch as branch_mod;
use weaver_core::discussion;

use crate::db::Db;
use crate::events;

use super::{require_branch, require_session};
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Artifacts — named, versioned documents stored in weaver.db. The GET resolves
// the content's references against the issue ledger (via smartdoc) and returns
// the projection alongside, so the SPA chips and `weaver artifact show` render
// the same join. Structure in the doc, state in the DB. See docs/artifacts.md.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct RevQuery {
    rev: Option<i64>,
}

/// The wire metadata for an artifact envelope.
fn artifact_meta(a: &Artifact) -> ArtifactMeta {
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
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_written",
        json!({ "name": a.name, "rev": a.rev, "title": a.title }),
    )
    .await
    .ok();
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
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_deleted",
        json!({ "name": a.name, "branch_id": a.branch_id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "deleted": true, "name": a.name })))
}

// ---------------------------------------------------------------------------
// Branch-scoped artifacts — the twin of the session-scoped routes above, for
// a `weaver artifact` target with no live session. `PUT` here creates the
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
    let existing = artifact::get(&st.db, &branch.repo_root, &branch.id, &name).await?;
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
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_written",
        json!({ "name": a.name, "rev": a.rev, "title": a.title }),
    )
    .await
    .ok();
    Ok(Json(
        artifact_view(&st.db, &branch.repo_root, &a, None).await?,
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
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "artifact_deleted",
        json!({ "name": a.name, "branch_id": a.branch_id }),
    )
    .await
    .ok();
    Ok(Json(json!({ "deleted": true, "name": a.name })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn test_state(db: Db) -> AppState {
        AppState {
            db: db.clone(),
            bus: crate::events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db),
        }
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
            State(st),
            Path((branch.id.clone(), "plan".to_string())),
            Json(ArtifactUpsertReq {
                content: "v2".to_string(),
                title: None,
                kind: None,
                author: None,
                repo: false,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(view.content, "v2");
        assert_eq!(view.meta.rev, 2);
        assert_eq!(view.meta.title, "The Plan", "title carries over unset");
        assert_eq!(view.versions[0].author, "agent");
    }
}
