//! Artifacts: named, versioned documents written *to weaver*, not the repo.
//!
//! An artifact is a design, report, diagram, or plan an agent (or the user)
//! hands the user — the outbound twin of `scratch/`. It belongs to a **repo**
//! (`repo_root`) and optionally a **branch** (`branch_id`); `branch_id IS NULL`
//! is *repo-shared* (the fan-out case: one plan, many child sessions), mirroring
//! the claimed-vs-backlog shape issues already have.
//!
//! Storage is two tables ([`crate::migrations`] 0007): an `artifacts` envelope
//! row and an append-only `artifact_versions` log. Every [`write`] appends a new
//! immutable revision — last write is a new `rev`, never a lost update — so
//! concurrent agent/user edits are safe and the viewer gets a version picker.
//!
//! Lookup resolves **branch-scoped first, then repo-shared**: a session's own
//! `plan` shadows the repo's shared `plan` of the same name. See
//! `docs/artifacts.md`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{now_iso, Db};

/// The artifact envelope plus its latest revision number — the row a list/get
/// returns. The content lives on a [`ArtifactVersion`]; this is the metadata.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Artifact {
    pub id: i64,
    pub repo_root: String,
    /// The branch that owns it, or `None` for a repo-shared artifact.
    pub branch_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub title: String,
    /// The latest revision number (joined from `artifact_versions`); 0 when the
    /// artifact somehow has no version (never, post-[`write`]).
    pub rev: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One immutable revision of an artifact's content.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ArtifactVersion {
    pub rev: i64,
    /// `agent` | `user` — who wrote this revision.
    pub author: String,
    pub content: String,
    pub created_at: String,
}

/// The columns of the envelope plus its latest `rev`, as one `SELECT`. Shared by
/// every read so the projected `rev` is computed one way.
const SELECT_WITH_REV: &str = "SELECT a.id, a.repo_root, a.branch_id, a.name, a.kind, a.title, \
     COALESCE((SELECT MAX(rev) FROM artifact_versions WHERE artifact_id = a.id), 0) AS rev, \
     a.created_at, a.updated_at \
     FROM artifacts a";

/// One revision to append, plus the scope/identity that targets (or creates) its
/// envelope. `branch_id = None` is the repo-shared scope; `author` is `"agent"`
/// or `"user"`.
#[derive(Debug, Clone, Default)]
pub struct NewRevision<'a> {
    pub repo_root: &'a str,
    pub branch_id: Option<&'a str>,
    pub name: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub author: &'a str,
}

/// Append a new revision of an artifact, creating the envelope on first write.
/// Branch-scoped when `branch_id` is `Some`, repo-shared when `None`. Returns
/// the persisted envelope with its new latest `rev`.
///
/// The next `rev` is `MAX(rev)+1` for the artifact (revisions start at 1). The
/// envelope's `title`/`kind` are updated to the latest values on every write, so
/// a later write can retitle without a separate verb; passing the existing
/// values is a no-op.
pub async fn write(db: &Db, new: &NewRevision<'_>) -> Result<Artifact> {
    write_resolved(
        db,
        &WriteRevision {
            repo_root: new.repo_root,
            scope: match new.branch_id {
                Some(id) => WriteScope::ExactBranch(id),
                None => WriteScope::Shared,
            },
            name: new.name,
            kind: Some(new.kind),
            title: Some(new.title),
            content: new.content,
            author: new.author,
            base_rev: None,
        },
    )
    .await
}

/// How a write resolves a name. Branch views fall back to the shared artifact;
/// internal callers can target an exact branch to create a shadowing artifact.
#[derive(Debug, Clone, Copy)]
pub enum WriteScope<'a> {
    Branch(&'a str),
    Shared,
    ExactBranch(&'a str),
}

/// An artifact edit, including its optional compare-and-swap prerequisite.
/// Omitted metadata preserves the resolved envelope's values.
pub struct WriteRevision<'a> {
    pub repo_root: &'a str,
    pub scope: WriteScope<'a>,
    pub name: &'a str,
    pub kind: Option<&'a str>,
    pub title: Option<&'a str>,
    pub content: &'a str,
    pub author: &'a str,
    pub base_rev: Option<i64>,
}

/// A rejected prerequisite. No storage changes have been made.
#[derive(Debug)]
pub struct RevisionConflict {
    pub latest: i64,
}

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stale artifact revision (latest: {})", self.latest)
    }
}

impl std::error::Error for RevisionConflict {}

/// Resolve identity, compare the prerequisite, preserve metadata and append
/// under one immediate transaction. Return this write's envelope snapshot,
/// even if another writer commits before the caller constructs its response.
pub async fn write_resolved(db: &Db, new: &WriteRevision<'_>) -> Result<Artifact> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let WriteRevision {
        repo_root,
        scope,
        name,
        kind,
        title,
        content,
        author,
        base_rev,
    } = *new;
    let (branch_id, fallback) = match scope {
        WriteScope::Branch(id) => (Some(id), true),
        WriteScope::ExactBranch(id) => (Some(id), false),
        WriteScope::Shared => (None, false),
    };
    let existing = sqlx::query_as::<_, Artifact>(&format!(
        "{SELECT_WITH_REV} WHERE a.repo_root = ? AND a.name = ?
         AND (a.branch_id IS ? OR (? AND a.branch_id IS NULL))
         ORDER BY a.branch_id IS NOT NULL DESC LIMIT 1"
    ))
    .bind(repo_root)
    .bind(name)
    .bind(branch_id)
    .bind(fallback)
    .fetch_optional(&mut *tx)
    .await?;
    let latest = existing.as_ref().map_or(0, |a| a.rev);
    if base_rev.is_some_and(|expected| expected != latest) {
        tx.rollback().await?;
        return Err(RevisionConflict { latest }.into());
    }
    let kind = kind
        .or_else(|| existing.as_ref().map(|a| a.kind.as_str()))
        .unwrap_or("markdown");
    let title = title
        .or_else(|| existing.as_ref().map(|a| a.title.as_str()))
        .unwrap_or_default();
    let branch_id = existing
        .as_ref()
        .map_or(branch_id, |a| a.branch_id.as_deref());
    let now = now_iso();
    let artifact_id = match &existing {
        Some(a) => {
            let id = a.id;
            sqlx::query("UPDATE artifacts SET kind = ?, title = ?, updated_at = ? WHERE id = ?")
                .bind(kind)
                .bind(title)
                .bind(&now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            id
        }
        None => {
            let artifact_id: i64 = sqlx::query_scalar(
                "UPDATE artifact_id_sequence
                 SET next_id = next_id + 1
                 WHERE singleton = 1
                 RETURNING next_id - 1",
            )
            .fetch_one(&mut *tx)
            .await?;
            let row: (i64,) = sqlx::query_as(
                "INSERT INTO artifacts
                    (id, repo_root, branch_id, name, kind, title, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(artifact_id)
            .bind(repo_root)
            .bind(branch_id)
            .bind(name)
            .bind(kind)
            .bind(title)
            .bind(&now)
            .bind(&now)
            .fetch_one(&mut *tx)
            .await?;
            row.0
        }
    };

    let next_rev = latest + 1;

    sqlx::query(
        "INSERT INTO artifact_versions (artifact_id, rev, author, content, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(artifact_id)
    .bind(next_rev)
    .bind(author)
    .bind(content)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    let persisted = sqlx::query_as::<_, Artifact>(&format!("{SELECT_WITH_REV} WHERE a.id = ?"))
        .bind(artifact_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(persisted)
}

/// Fetch one envelope by id (with its latest `rev`).
pub async fn get_by_id(db: &Db, id: i64) -> Result<Option<Artifact>> {
    let row = sqlx::query_as::<_, Artifact>(&format!("{SELECT_WITH_REV} WHERE a.id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

/// Resolve an artifact by name in a session's view: **branch-scoped first, then
/// repo-shared**. A branch's own `plan` shadows the repo's shared `plan`.
pub async fn get(
    db: &Db,
    repo_root: &str,
    branch_id: &str,
    name: &str,
) -> Result<Option<Artifact>> {
    if let Some(a) = sqlx::query_as::<_, Artifact>(&format!(
        "{SELECT_WITH_REV} WHERE a.repo_root = ? AND a.branch_id = ? AND a.name = ?"
    ))
    .bind(repo_root)
    .bind(branch_id)
    .bind(name)
    .fetch_optional(db)
    .await?
    {
        return Ok(Some(a));
    }
    get_shared(db, repo_root, name).await
}

/// Resolve a repo-shared artifact by name (`branch_id IS NULL`).
pub async fn get_shared(db: &Db, repo_root: &str, name: &str) -> Result<Option<Artifact>> {
    let row = sqlx::query_as::<_, Artifact>(&format!(
        "{SELECT_WITH_REV} WHERE a.repo_root = ? AND a.branch_id IS NULL AND a.name = ?"
    ))
    .bind(repo_root)
    .bind(name)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// One revision's content, by artifact id and rev. `None` when that rev does not
/// exist.
pub async fn version(db: &Db, artifact_id: i64, rev: i64) -> Result<Option<ArtifactVersion>> {
    let row = sqlx::query_as::<_, ArtifactVersion>(
        "SELECT rev, author, content, created_at FROM artifact_versions
         WHERE artifact_id = ? AND rev = ?",
    )
    .bind(artifact_id)
    .bind(rev)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// The latest revision's content for an artifact. `None` when it has no version.
pub async fn latest_version(db: &Db, artifact_id: i64) -> Result<Option<ArtifactVersion>> {
    let row = sqlx::query_as::<_, ArtifactVersion>(
        "SELECT rev, author, content, created_at FROM artifact_versions
         WHERE artifact_id = ? ORDER BY rev DESC LIMIT 1",
    )
    .bind(artifact_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// The full version history of an artifact, newest first (metadata only — no
/// content, which the viewer fetches per-rev).
pub async fn history(db: &Db, artifact_id: i64) -> Result<Vec<ArtifactVersion>> {
    let rows = sqlx::query_as::<_, ArtifactVersion>(
        "SELECT rev, author, '' AS content, created_at FROM artifact_versions
         WHERE artifact_id = ? ORDER BY rev DESC",
    )
    .bind(artifact_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The artifacts visible from a session: this branch's plus the repo-shared
/// ones, latest rev each. A branch-scoped artifact **shadows** a shared one of
/// the same name (the resolution [`get`] uses), so the listing shows the scoped
/// row and drops the duplicate shared name.
pub async fn list_for_session(db: &Db, repo_root: &str, branch_id: &str) -> Result<Vec<Artifact>> {
    let mut scoped = sqlx::query_as::<_, Artifact>(&format!(
        "{SELECT_WITH_REV} WHERE a.repo_root = ? AND a.branch_id = ? ORDER BY a.name ASC"
    ))
    .bind(repo_root)
    .bind(branch_id)
    .fetch_all(db)
    .await?;

    let shared = sqlx::query_as::<_, Artifact>(&format!(
        "{SELECT_WITH_REV} WHERE a.repo_root = ? AND a.branch_id IS NULL ORDER BY a.name ASC"
    ))
    .bind(repo_root)
    .fetch_all(db)
    .await?;

    let scoped_names: std::collections::HashSet<String> =
        scoped.iter().map(|a| a.name.clone()).collect();
    for s in shared {
        if !scoped_names.contains(&s.name) {
            scoped.push(s);
        }
    }
    scoped.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(scoped)
}

/// Every artifact in a repo, regardless of scope (`artifact ls --repo`), latest
/// rev each. Branch-scoped rows sort after shared ones by name.
pub async fn list_for_repo(db: &Db, repo_root: &str) -> Result<Vec<Artifact>> {
    let rows = sqlx::query_as::<_, Artifact>(&format!(
        "{SELECT_WITH_REV} WHERE a.repo_root = ? ORDER BY a.name ASC, a.branch_id IS NOT NULL"
    ))
    .bind(repo_root)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Delete an artifact and its entire revision history, by envelope id. Removing
/// an artifact removes the whole document — every snapshot goes with it.
///
/// Foreign keys aren't enabled on the pool, so the `artifact_versions` cascade
/// won't fire — clear the versions explicitly before dropping the envelope.
pub async fn delete(db: &Db, id: i64) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
    sqlx::query("DELETE FROM artifact_versions WHERE artifact_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM artifacts WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        crate::db::connect_in_memory().await.unwrap()
    }

    /// Ensure a `branches` row with the given id exists, so a branch-scoped
    /// write satisfies the `artifacts.branch_id` foreign key. Idempotent.
    async fn ensure_branch(db: &Db, id: &str) {
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch, base_branch)
             VALUES (?, '/r', ?, 'main') ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    }

    /// Append a revision, with the common defaults filled in. Seeds the owning
    /// branch row when the write is branch-scoped.
    async fn wr(
        db: &Db,
        branch_id: Option<&str>,
        name: &str,
        title: &str,
        content: &str,
        author: &str,
    ) -> Artifact {
        if let Some(bid) = branch_id {
            ensure_branch(db, bid).await;
        }
        write(
            db,
            &NewRevision {
                repo_root: "/r",
                branch_id,
                name,
                kind: "markdown",
                title,
                content,
                author,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn write_creates_then_increments_revisions() {
        let db = db().await;
        let a = wr(&db, Some("b1"), "plan", "Plan", "v1", "agent").await;
        assert_eq!(a.rev, 1);
        assert_eq!(a.name, "plan");
        assert_eq!(a.title, "Plan");
        assert_eq!(a.branch_id.as_deref(), Some("b1"));

        let a2 = wr(&db, Some("b1"), "plan", "Plan v2", "v2", "user").await;
        // Same envelope (same id), next revision, retitled.
        assert_eq!(a2.id, a.id);
        assert_eq!(a2.rev, 2);
        assert_eq!(a2.title, "Plan v2");

        // Both revisions are addressable, with their content and author.
        let v1 = version(&db, a.id, 1).await.unwrap().unwrap();
        assert_eq!(v1.content, "v1");
        assert_eq!(v1.author, "agent");
        let v2 = latest_version(&db, a.id).await.unwrap().unwrap();
        assert_eq!(v2.rev, 2);
        assert_eq!(v2.content, "v2");
        assert_eq!(v2.author, "user");

        // History is newest-first, metadata only (no content).
        let hist = history(&db, a.id).await.unwrap();
        assert_eq!(hist.iter().map(|h| h.rev).collect::<Vec<_>>(), vec![2, 1]);
        assert!(hist.iter().all(|h| h.content.is_empty()));
    }

    #[tokio::test]
    async fn branch_scoped_shadows_repo_shared_on_get() {
        let db = db().await;
        // A repo-shared `plan` and a branch's own `plan` of the same name.
        wr(&db, None, "plan", "Shared", "shared", "agent").await;
        wr(&db, Some("b1"), "plan", "Mine", "mine", "agent").await;

        // From b1's view, `get` resolves the branch-scoped one.
        let got = get(&db, "/r", "b1", "plan").await.unwrap().unwrap();
        assert_eq!(got.title, "Mine");
        assert_eq!(got.branch_id.as_deref(), Some("b1"));

        // From another branch with no own `plan`, `get` falls back to shared.
        let other = get(&db, "/r", "b2", "plan").await.unwrap().unwrap();
        assert_eq!(other.title, "Shared");
        assert!(other.branch_id.is_none());

        // `get_shared` always resolves the shared one.
        let shared = get_shared(&db, "/r", "plan").await.unwrap().unwrap();
        assert_eq!(shared.title, "Shared");
    }

    #[tokio::test]
    async fn list_for_session_merges_scoped_and_shared_dropping_shadowed() {
        let db = db().await;
        wr(&db, None, "plan", "Shared plan", "x", "agent").await;
        wr(&db, None, "report", "Shared report", "x", "agent").await;
        wr(&db, Some("b1"), "plan", "My plan", "x", "agent").await;
        wr(&db, Some("b1"), "design", "My design", "x", "agent").await;

        let listed = list_for_session(&db, "/r", "b1").await.unwrap();
        // plan (scoped, shadows shared), report (shared), design (scoped).
        let names: Vec<&str> = listed.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["design", "plan", "report"]);
        // The `plan` shown is the branch-scoped one, not the shared.
        let plan = listed.iter().find(|a| a.name == "plan").unwrap();
        assert_eq!(plan.title, "My plan");
        assert_eq!(plan.branch_id.as_deref(), Some("b1"));
    }

    #[tokio::test]
    async fn list_for_repo_spans_every_scope() {
        let db = db().await;
        wr(&db, None, "plan", "", "x", "agent").await;
        wr(&db, Some("b1"), "plan", "", "x", "agent").await;
        write(
            &db,
            &NewRevision {
                repo_root: "/other",
                branch_id: None,
                name: "plan",
                kind: "markdown",
                title: "",
                content: "x",
                author: "agent",
            },
        )
        .await
        .unwrap();
        let all = list_for_repo(&db, "/r").await.unwrap();
        assert_eq!(
            all.len(),
            2,
            "two `plan` rows in /r (shared + b1), none from /other"
        );
    }

    #[tokio::test]
    async fn scoped_and_shared_same_name_are_distinct_rows() {
        let db = db().await;
        let shared = wr(&db, None, "plan", "", "s", "agent").await;
        let scoped = wr(&db, Some("b1"), "plan", "", "m", "agent").await;
        assert_ne!(shared.id, scoped.id);
        // Writing the shared one again appends to *its* lineage, not the scoped.
        let shared2 = wr(&db, None, "plan", "", "s2", "agent").await;
        assert_eq!(shared2.id, shared.id);
        assert_eq!(shared2.rev, 2);
        // The scoped one is still at rev 1.
        let scoped_now = get(&db, "/r", "b1", "plan").await.unwrap().unwrap();
        assert_eq!(scoped_now.rev, 1);
    }

    #[tokio::test]
    async fn delete_removes_envelope_and_every_revision() {
        let db = db().await;
        // A shared `plan` and a same-named branch-scoped one, the scoped with
        // history, so we can prove delete is targeted and takes all revisions.
        let shared = wr(&db, None, "plan", "Shared", "s", "agent").await;
        wr(&db, Some("b1"), "plan", "Mine v1", "v1", "agent").await;
        let scoped = wr(&db, Some("b1"), "plan", "Mine v2", "v2", "user").await;
        assert_eq!(scoped.rev, 2);

        delete(&db, scoped.id).await.unwrap();

        // The scoped envelope and both its revisions are gone.
        assert!(get_by_id(&db, scoped.id).await.unwrap().is_none());
        assert!(version(&db, scoped.id, 1).await.unwrap().is_none());
        assert!(version(&db, scoped.id, 2).await.unwrap().is_none());
        assert!(history(&db, scoped.id).await.unwrap().is_empty());

        // The shared `plan` is untouched — `get` from b1 now falls back to it.
        let resolved = get(&db, "/r", "b1", "plan").await.unwrap().unwrap();
        assert_eq!(resolved.id, shared.id);
        assert_eq!(resolved.title, "Shared");
    }
}
