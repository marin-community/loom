//! Recently-used repositories (`recent_repos`) and the managed repo store
//! (`repos`) loom clones into a container-owned root.

use anyhow::Result;
use serde::Serialize;
use sqlx::FromRow;
use std::path::{Path, PathBuf};

use crate::db::{now_iso, Db};
use weaver_core::git;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RecentRepo {
    pub repo_root: String,
    pub last_used_at: String,
    /// How many tracked branches exist in this repo (may be zero).
    pub active_branches: i64,
}

pub async fn record_use(db: &Db, repo_root: &str) -> Result<()> {
    tracing::debug!(repo_root, "recording recent repo use");
    sqlx::query(
        "INSERT INTO recent_repos (repo_root, last_used_at) VALUES (?, ?)
         ON CONFLICT(repo_root) DO UPDATE SET last_used_at = excluded.last_used_at",
    )
    .bind(repo_root)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn recent(db: &Db, limit: i64) -> Result<Vec<RecentRepo>> {
    let rows = sqlx::query_as::<_, RecentRepo>(
        "SELECT r.repo_root,
                r.last_used_at,
                (SELECT COUNT(*) FROM branches b WHERE b.repo_root = r.repo_root)
                    AS active_branches
         FROM recent_repos r
         ORDER BY r.last_used_at DESC
         LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    tracing::debug!(count = rows.len(), "listed recent repos");
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Managed repo store — repos loom clones into a container-owned root and may
// launch sessions against. The `repos` table doubles as the clone allowlist:
// only a registered slug may be resolved and cloned (the boundary the future
// GitHub trigger relies on). See the shared-loom design, §6.2.
// ---------------------------------------------------------------------------

/// The managed repo root: `WEAVER_REPOS_DIR`, else `$WEAVER_HOME/repos`. Clones
/// are laid out `<root>/<owner>/<name>`.
pub fn repos_dir() -> PathBuf {
    if let Ok(p) = std::env::var("WEAVER_REPOS_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    weaver_core::db::weaver_home().join("repos")
}

/// A validated GitHub `owner/name` slug. The on-disk managed path is derived
/// from this, so each component is strictly validated against path traversal
/// before a `RepoSlug` can be constructed (see [`parse_slug`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSlug {
    pub owner: String,
    pub name: String,
}

impl RepoSlug {
    /// The canonical `owner/name` form — the `repos` table primary key.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
    /// The managed clone path under `root` (`<root>/<owner>/<name>`).
    pub fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.owner).join(&self.name)
    }
    /// The canonical GitHub HTTPS clone URL for a bare `owner/name` reference.
    pub fn github_url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.name)
    }
}

/// Whether a (trimmed) repo reference is a URL/scp form rather than a bare
/// `owner/name` slug.
fn is_url_ref(s: &str) -> bool {
    s.contains("://") || (s.contains('@') && s.contains(':'))
}

/// One path component is a safe repo identifier segment: non-empty, not a `.` or
/// `..` traversal element, and limited to GitHub's owner/name charset. This is
/// the gate that makes `<root>/<owner>/<name>` impossible to escape.
fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Parse a repo reference — a bare `owner/name` slug or a GitHub URL — into a
/// validated [`RepoSlug`]. The bare form must be *exactly* `owner/name`: this is
/// the untrusted input the create path and GitHub trigger accept, so anything
/// else (`..`, an absolute path, extra segments) is rejected. A URL form
/// (`https://…`, `git@…:…`, `file://…`) is reduced to the trailing two path
/// components. Either way both components are strictly validated, so the derived
/// path can never escape the managed root. Returns the rejection reason on
/// failure (the caller maps it to a 400).
pub fn parse_slug(input: &str) -> Result<RepoSlug, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("repo identifier is empty".into());
    }
    let reject = || format!("'{input}' is not a clean owner/name repo identifier");

    // A URL/scp reference is reduced to its path; a bare slug is used verbatim.
    let (owner, name) = if is_url_ref(trimmed) {
        let path_part = if let Some((_, rest)) = trimmed.split_once("://") {
            // scheme://host/owner/name → drop the host segment.
            rest.split_once('/').map(|(_, p)| p).unwrap_or("")
        } else {
            // scp form git@host:owner/name → keep the part after the last ':'.
            trimmed.rsplit_once(':').map(|(_, p)| p).unwrap_or("")
        };
        let path_part = path_part.trim_matches('/');
        let path_part = path_part.strip_suffix(".git").unwrap_or(path_part);
        let segs: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
        match segs.as_slice() {
            [.., o, n] => (o.to_string(), n.to_string()),
            _ => return Err(reject()),
        }
    } else {
        // Bare form: exactly `owner/name` — no leading slash (absolute), no extra
        // segments. `split('/')` keeps empties, so `/abs`, `a/b/c`, and a trailing
        // slash all fail the two-element match.
        let s = trimmed.strip_suffix(".git").unwrap_or(trimmed);
        match s.split('/').collect::<Vec<_>>().as_slice() {
            [o, n] => (o.to_string(), n.to_string()),
            _ => return Err(reject()),
        }
    };
    if !valid_segment(&owner) || !valid_segment(&name) {
        return Err(reject());
    }
    Ok(RepoSlug { owner, name })
}

/// The clone URL to register for a repo reference: a bare `owner/name` resolves
/// to its canonical GitHub HTTPS remote; an explicit URL is kept as given.
pub fn remote_url_for(input: &str, slug: &RepoSlug) -> String {
    let trimmed = input.trim();
    if is_url_ref(trimmed) {
        trimmed.to_string()
    } else {
        slug.github_url()
    }
}

/// A repo registered in the managed store — the slug → (remote, path) mapping
/// that also serves as the clone allowlist.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ManagedRepo {
    /// Canonical GitHub `owner/name`.
    pub slug: String,
    /// The clone source URL.
    pub remote_url: String,
    /// The managed on-disk clone path.
    pub path: String,
    pub created_at: String,
}

/// Register (or update the remote/path of) a managed repo, returning the row.
pub async fn register(db: &Db, slug: &str, remote_url: &str, path: &str) -> Result<ManagedRepo> {
    sqlx::query(
        "INSERT INTO repos (slug, remote_url, path) VALUES (?, ?, ?)
         ON CONFLICT(slug) DO UPDATE SET
             remote_url = excluded.remote_url,
             path = excluded.path",
    )
    .bind(slug)
    .bind(remote_url)
    .bind(path)
    .execute(db)
    .await?;
    get_registered(db, slug)
        .await?
        .ok_or_else(|| anyhow::anyhow!("registered repo '{slug}' vanished"))
}

/// Every registered repo, newest first — the clone allowlist.
pub async fn list_registered(db: &Db) -> Result<Vec<ManagedRepo>> {
    let rows = sqlx::query_as::<_, ManagedRepo>(
        "SELECT slug, remote_url, path, created_at FROM repos ORDER BY created_at DESC, slug",
    )
    .fetch_all(db)
    .await?;
    tracing::debug!(count = rows.len(), "listed managed repos");
    Ok(rows)
}

/// One registered repo by slug, or `None` when it is not in the allowlist.
pub async fn get_registered(db: &Db, slug: &str) -> Result<Option<ManagedRepo>> {
    let row = sqlx::query_as::<_, ManagedRepo>(
        "SELECT slug, remote_url, path, created_at FROM repos WHERE slug = ?",
    )
    .bind(slug)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Whether `repo_root` is an allowlisted (registered) managed repo — the gate
/// that decides whether a session's committed `.weaver/config.toml` `[setup]`
/// script may run. A registered repo's stored `path` is the managed clone
/// (`<repos_dir>/<owner>/<name>`); the launch resolves the worktree's repo root
/// by canonicalizing that path, so we compare canonicalized paths on both sides.
/// A repo not in the `repos` table (e.g. a local bind-mounted checkout) is not
/// allowlisted, and its setup script is never executed (the design's privileged
/// code-execution boundary, §6.4).
pub async fn is_allowlisted(db: &Db, repo_root: &Path) -> Result<bool> {
    let target = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    Ok(list_registered(db).await?.into_iter().any(|registered| {
        let path = PathBuf::from(&registered.path);
        path.canonicalize().unwrap_or(path) == target
    }))
}

/// How a managed-repo resolution can fail, so the web layer maps each cause to
/// the right HTTP status.
#[derive(Debug)]
pub enum ResolveError {
    /// A malformed identifier or one not in the allowlist — the caller's fault (400).
    BadRequest(String),
    /// The clone/fetch itself failed (e.g. the remote is unreachable) — upstream (502).
    Clone(String),
}

/// Resolve a managed-repo reference to its on-disk clone, ensuring it is present.
/// `input` is a slug or URL; it is parsed strictly (traversal rejected), looked
/// up in the registered allowlist (an unregistered repo is refused — the trigger
/// boundary), then cloned-if-absent / fetched. Returns the managed checkout path.
pub async fn resolve_clone(db: &Db, input: &str) -> std::result::Result<PathBuf, ResolveError> {
    let slug = parse_slug(input).map_err(ResolveError::BadRequest)?;
    let slug_str = slug.slug();
    let registered = get_registered(db, &slug_str)
        .await
        .map_err(|e| ResolveError::Clone(e.to_string()))?
        .ok_or_else(|| {
            ResolveError::BadRequest(format!(
                "repo '{slug_str}' is not registered — register it via POST /api/repos first"
            ))
        })?;
    let dest = PathBuf::from(&registered.path);
    git::clone(&registered.remote_url, &dest)
        .await
        .map_err(|e| ResolveError::Clone(format!("cloning {slug_str}: {e}")))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_in_memory;
    use weaver_core::branch;

    #[tokio::test]
    async fn records_and_orders_by_recency() {
        let db = connect_in_memory().await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        record_use(&db, "/code/beta").await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        let rows = recent(&db, 10).await.unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.repo_root.as_str()).collect();
        assert_eq!(paths, ["/code/alpha", "/code/beta"]);
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn counts_tracked_branches() {
        let db = connect_in_memory().await.unwrap();
        record_use(&db, "/code/alpha").await.unwrap();
        branch::upsert(&db, "/code/alpha", "main", "main")
            .await
            .unwrap();
        branch::upsert(&db, "/code/alpha", "feature", "main")
            .await
            .unwrap();
        let rows = recent(&db, 10).await.unwrap();
        assert_eq!(rows[0].active_branches, 2);
    }

    #[tokio::test]
    async fn limit_caps_the_result() {
        let db = connect_in_memory().await.unwrap();
        for i in 0..5 {
            record_use(&db, &format!("/code/r{i}")).await.unwrap();
        }
        assert_eq!(recent(&db, 3).await.unwrap().len(), 3);
    }

    #[test]
    fn parse_slug_accepts_clean_owner_name_and_urls() {
        // The bare canonical form.
        let s = parse_slug("acme/widgets").unwrap();
        assert_eq!((s.owner.as_str(), s.name.as_str()), ("acme", "widgets"));
        assert_eq!(s.slug(), "acme/widgets");
        assert_eq!(s.github_url(), "https://github.com/acme/widgets.git");
        assert_eq!(
            s.path(Path::new("/srv/repos")),
            Path::new("/srv/repos/acme/widgets")
        );

        // URL forms reduce to their trailing owner/name; `.git` is stripped.
        for url in [
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "http://github.com/acme/widgets/",
            "git@github.com:acme/widgets.git",
            "file:///tmp/store/acme/widgets",
        ] {
            assert_eq!(
                parse_slug(url).unwrap().slug(),
                "acme/widgets",
                "url: {url}"
            );
        }

        // A dot is legal inside a name (e.g. `repo.js`).
        assert_eq!(parse_slug("acme/repo.js").unwrap().name, "repo.js");
    }

    #[test]
    fn parse_slug_rejects_traversal_and_malformed() {
        for bad in [
            "",
            "owner",                          // one segment
            "a/b/c",                          // three segments
            "owner/name/",                    // trailing slash
            "/etc/passwd",                    // absolute path
            "../etc",                         // parent traversal
            "owner/..",                       // traversal in name
            "../../root",                     // deep traversal
            "owner/na me",                    // space (bad charset)
            "https://github.com/a/../../etc", // traversal in url tail
        ] {
            assert!(parse_slug(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[tokio::test]
    async fn register_lists_and_gets() {
        let db = connect_in_memory().await.unwrap();
        let r = register(
            &db,
            "acme/widgets",
            "https://example/acme/widgets.git",
            "/srv/acme/widgets",
        )
        .await
        .unwrap();
        assert_eq!(r.slug, "acme/widgets");
        assert_eq!(r.remote_url, "https://example/acme/widgets.git");
        assert_eq!(r.path, "/srv/acme/widgets");

        assert!(get_registered(&db, "acme/widgets").await.unwrap().is_some());
        assert!(get_registered(&db, "ghost/repo").await.unwrap().is_none());
        assert_eq!(list_registered(&db).await.unwrap().len(), 1);

        // Re-registering the same slug updates the remote/path, not a duplicate.
        register(
            &db,
            "acme/widgets",
            "https://example/acme/widgets2.git",
            "/srv/acme/w2",
        )
        .await
        .unwrap();
        let rows = list_registered(&db).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].remote_url, "https://example/acme/widgets2.git");
    }

    /// `resolve_clone` refuses an unregistered repo (the allowlist boundary) and a
    /// traversal identifier, and on a registered repo clones it from the (local,
    /// no-network) remote into the managed path.
    #[tokio::test]
    async fn resolve_clone_enforces_allowlist_then_clones() {
        let db = connect_in_memory().await.unwrap();

        // Unregistered → BadRequest, no clone attempted.
        assert!(matches!(
            resolve_clone(&db, "ghost/repo").await,
            Err(ResolveError::BadRequest(_))
        ));
        // Traversal → BadRequest before any lookup.
        assert!(matches!(
            resolve_clone(&db, "../etc").await,
            Err(ResolveError::BadRequest(_))
        ));

        // Build a local bare repo to act as the remote (no network).
        let src = tempfile::tempdir().unwrap();
        let sdir = src.path();
        run_git(sdir, &["init", "-q", "-b", "main"]).await;
        run_git(sdir, &["config", "user.email", "t@t.t"]).await;
        run_git(sdir, &["config", "user.name", "t"]).await;
        std::fs::write(sdir.join("README.md"), "hi\n").unwrap();
        run_git(sdir, &["add", "-A"]).await;
        run_git(sdir, &["commit", "-q", "-m", "init"]).await;
        let bare = tempfile::tempdir().unwrap();
        let bare_path = bare.path().join("origin.git");
        run_git(
            sdir,
            &[
                "clone",
                "--bare",
                "-q",
                &sdir.to_string_lossy(),
                &bare_path.to_string_lossy(),
            ],
        )
        .await;

        // Register the slug pointing at the local bare remote, with a managed dest.
        let dest_root = tempfile::tempdir().unwrap();
        let dest = dest_root.path().join("acme").join("widgets");
        register(
            &db,
            "acme/widgets",
            &bare_path.to_string_lossy(),
            &dest.to_string_lossy(),
        )
        .await
        .unwrap();

        let path = resolve_clone(&db, "acme/widgets").await.unwrap();
        assert_eq!(path, dest);
        assert!(
            dest.join(".git").exists(),
            "repo was cloned to the managed path"
        );
        // Idempotent: a second resolve fetches into the existing clone.
        resolve_clone(&db, "acme/widgets").await.unwrap();
    }

    /// Run `git` in `dir` for the resolve test (the crate has no test-only git
    /// helper of its own; `weaver_core::git` exposes only typed operations).
    async fn run_git(dir: &Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
