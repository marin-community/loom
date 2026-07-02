//! Thin async wrapper over the `git` binary.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiffStat {
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_with_envs(dir, args, &[]).await
}

/// Same as [`git`], plus extra environment variables for the subprocess — how a
/// short-lived credential is threaded into one invocation (see
/// [`token_auth_envs`]) without ever appearing in `args` (which the failure log
/// below prints).
async fn git_with_envs(dir: &Path, args: &[&str], envs: &[(&str, String)]) -> Result<String> {
    tracing::debug!(?args, dir = %dir.display(), "running git");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
        .output()
        .await
        .context("failed to spawn git")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        tracing::warn!(
            args = %args.join(" "),
            dir = %dir.display(),
            code = out.status.code().unwrap_or(-1),
            stderr = %truncate(stderr.trim(), 500),
            stdout = %truncate(stdout.trim(), 500),
            "git failed"
        );
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Truncate a string to `max` chars for log output, appending an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push_str("...[truncated]");
        t
    }
}

/// Absolute path to the **main** working tree of the repository containing
/// `dir`. When `dir` is inside a linked worktree this still resolves back to
/// the primary checkout, so weaver's worktrees never nest inside each other.
pub async fn repo_root(dir: &Path) -> Result<PathBuf> {
    let out = git(dir, &["worktree", "list", "--porcelain"])
        .await
        .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
    // `git worktree list` always reports the main working tree first.
    let first = out
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .ok_or_else(|| anyhow!("could not determine the main worktree"))?;
    Ok(PathBuf::from(first))
}

/// Environment variables that make one git invocation authenticate with
/// `token` over HTTPS, via git's env-var config mechanism
/// (`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`, since git 2.31) rather than a CLI
/// arg or a persisted `.git/config` entry — so the token never appears in
/// `argv` (visible to `ps`, and to the `args`-logging `git()`/`git_with_envs`
/// failure path above), a process listing, or the repo's remote URL. Scoped to
/// this one subprocess only; nothing is written to disk.
fn token_auth_envs(token: &str) -> Vec<(&'static str, String)> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let header = format!(
        "AUTHORIZATION: basic {}",
        STANDARD.encode(format!("x-access-token:{token}"))
    );
    vec![
        ("GIT_CONFIG_COUNT", "1".to_string()),
        ("GIT_CONFIG_KEY_0", "http.extraheader".to_string()),
        ("GIT_CONFIG_VALUE_0", header),
    ]
}

/// Clone `url` into `dest`, or fetch into it when `dest` is already a clone —
/// idempotent, so two session launches racing to acquire the same managed repo
/// converge instead of one erroring on a populated directory.
///
/// `token` authenticates this one clone/fetch with a caller-supplied credential
/// (e.g. a GitHub App installation token) via [`token_auth_envs`]. Pass `None`
/// to fall back to the ambient git credential helper (the deploy image wires
/// `GH_TOKEN`) — the only behavior before per-call tokens existed, still used
/// for repos with no App installation.
pub async fn clone(url: &str, dest: &Path, token: Option<&str>) -> Result<()> {
    let envs = token.map(token_auth_envs).unwrap_or_default();
    // An existing clone (its `.git` is present) is refreshed, not re-cloned.
    if dest.join(".git").exists() {
        git_with_envs(dest, &["fetch", "--all", "--prune"], &envs).await?;
        tracing::info!(dest = %dest.display(), "fetched existing clone");
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating repo parent {}", parent.display()))?;
    }
    // `git clone` has no working tree to `-C` into yet, so it is spawned directly
    // rather than through the `git()` helper.
    let dest_str = dest.to_string_lossy();
    let out = Command::new("git")
        .args(["clone", url, &dest_str])
        .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
        .output()
        .await
        .context("failed to spawn git clone")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        tracing::warn!(
            url,
            dest = %dest.display(),
            stderr = %truncate(stderr.trim(), 500),
            "git clone failed"
        );
        bail!("git clone failed: {}", stderr.trim());
    }
    tracing::info!(url, dest = %dest.display(), "cloned repo");
    Ok(())
}

/// Ensure `pattern` is present in the repository's `.git/info/exclude`, so
/// that (for example) the in-repo `.worktrees/` directory is not reported as
/// untracked content. This is local-only and never touches a tracked file.
pub async fn ensure_excluded(repo_root: &Path, pattern: &str) -> Result<()> {
    let common = git(
        repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let info = PathBuf::from(common).join("info");
    tokio::fs::create_dir_all(&info).await.ok();
    let exclude = info.join("exclude");
    let current = tokio::fs::read_to_string(&exclude)
        .await
        .unwrap_or_default();
    if current.lines().any(|l| l.trim() == pattern) {
        return Ok(());
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(pattern);
    next.push('\n');
    tokio::fs::write(&exclude, next).await?;
    tracing::info!(repo = %repo_root.display(), pattern = %pattern, "exclude pattern added");
    Ok(())
}

pub async fn current_branch(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await
}

pub async fn branch_exists(dir: &Path, branch: &str) -> bool {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .is_ok()
}

/// Whether a remote named `name` is configured (`git remote get-url`).
async fn has_remote(dir: &Path, name: &str) -> bool {
    git(dir, &["remote", "get-url", name]).await.is_ok()
}

/// Whether `rev` resolves to a commit in this repo.
async fn commit_exists(dir: &Path, rev: &str) -> bool {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .await
    .is_ok()
}

/// The default branch on `origin` (e.g. `main`), resolved locally and cheaply:
/// the recorded `origin/HEAD` symref, else a probe of the usual names against
/// the existing remote-tracking refs. `None` when none of those resolve (a
/// fresh clone with no `origin/HEAD` and no `main`/`master` tracking ref).
async fn origin_default_branch(dir: &Path) -> Option<String> {
    if let Ok(out) = git(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    {
        if let Some(name) = out.strip_prefix("origin/") {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    for cand in ["main", "master"] {
        if commit_exists(dir, &format!("origin/{cand}")).await {
            return Some(cand.to_string());
        }
    }
    None
}

/// The base a freshly-launched session should fork from: the repo's default
/// branch on `origin`, **fetched fresh**, so new work always starts from the
/// latest mainline rather than whatever the launching checkout happens to have
/// checked out (and possibly stale). Returns the `origin/<default>` ref, which
/// the diff machinery already treats as a first-class base (see
/// [`merge_base_fresh`]).
///
/// Degrades gracefully: with no `origin` remote (a local-only repo, or the test
/// harness) it falls back to the local current branch, the historical default.
/// A failed fetch (offline) is non-fatal — the existing `origin/<default>`
/// tracking ref is used as-is; only if that ref doesn't resolve at all do we
/// fall back to the current branch.
pub async fn default_base(dir: &Path) -> Result<String> {
    if !has_remote(dir, "origin").await {
        return current_branch(dir).await;
    }
    if let Some(default) = origin_default_branch(dir).await {
        // Best-effort: refresh the tracking ref so the fork point is current.
        // Ignore network failures — a stale ref still beats the local branch.
        let _ = git(dir, &["fetch", "origin", &default]).await;
        let remote_ref = format!("origin/{default}");
        if commit_exists(dir, &remote_ref).await {
            return Ok(remote_ref);
        }
    }
    current_branch(dir).await
}

/// Create a new worktree at `path` on a new `branch` forked from `base`.
pub async fn worktree_add(repo_root: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    let path = path.to_string_lossy();
    git(repo_root, &["worktree", "add", "-b", branch, &path, base]).await?;
    tracing::info!(%branch, base, path = %path, "worktree created");
    Ok(())
}

/// Check out an existing `branch` into a new worktree at `path` (no `-b`).
pub async fn worktree_add_existing(repo_root: &Path, path: &Path, branch: &str) -> Result<()> {
    let path = path.to_string_lossy();
    git(repo_root, &["worktree", "add", &path, branch]).await?;
    tracing::info!(%branch, path = %path, "worktree created for existing branch");
    Ok(())
}

/// Materialize a local `branch` from `origin/<branch>` — used to check out a PR's
/// head branch after a fetch, since a bare branch name resolves only local heads.
/// A no-op when the local branch already exists; errors if `origin/<branch>` was
/// never fetched. Creating from a remote-tracking ref sets upstream, so a later
/// `git push` targets the same branch (updating the PR).
pub async fn create_local_branch_from_origin(repo_root: &Path, branch: &str) -> Result<()> {
    if branch_exists(repo_root, branch).await {
        return Ok(());
    }
    git(repo_root, &["branch", branch, &format!("origin/{branch}")]).await?;
    tracing::info!(%branch, "created local branch from origin");
    Ok(())
}

/// List local branch names (`refs/heads/*`).
pub async fn list_branches(repo_root: &Path) -> Result<Vec<String>> {
    let out = git(
        repo_root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .await?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Path of the worktree that currently has `branch` checked out, if any.
/// Parses `git worktree list --porcelain`: each record is a `worktree <path>`
/// line optionally followed by `HEAD <sha>` and either `branch refs/heads/<n>`
/// or `detached`. Records are separated by blank lines.
pub async fn worktree_for_branch(repo_root: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let out = git(repo_root, &["worktree", "list", "--porcelain"]).await?;
    let target = format!("refs/heads/{branch}");
    let mut current: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            if rest == target {
                return Ok(current);
            }
        } else if line.is_empty() {
            current = None;
        }
    }
    Ok(None)
}

pub async fn worktree_remove(repo_root: &Path, path: &Path) -> Result<()> {
    let path = path.to_string_lossy();
    git(repo_root, &["worktree", "remove", "--force", &path]).await?;
    tracing::info!(path = %path, "worktree removed");
    Ok(())
}

/// Prune stale worktree administrative entries (`git worktree prune`) — drop
/// registrations whose working directory is gone, so a later `worktree_add*` at
/// the same path is not rejected as "already registered". Idempotent: a no-op
/// when nothing is stale. Used when re-checking out a branch whose worktree was
/// torn down (e.g. recovering an archived session).
pub async fn worktree_prune(repo_root: &Path) -> Result<()> {
    git(repo_root, &["worktree", "prune"]).await?;
    tracing::info!(repo = %repo_root.display(), "worktree pruned");
    Ok(())
}

pub async fn delete_branch(repo_root: &Path, branch: &str) -> Result<()> {
    git(repo_root, &["branch", "-D", branch]).await?;
    tracing::info!(%branch, "branch deleted");
    Ok(())
}

/// Merge-base SHA between `base` and the worktree's `HEAD`.
pub async fn merge_base(work_dir: &Path, base: &str) -> Result<String> {
    git(work_dir, &["merge-base", base, "HEAD"]).await
}

/// Is `a` an ancestor of `b`? (`git merge-base --is-ancestor` exits 0 when so.)
async fn is_ancestor(work_dir: &Path, a: &str, b: &str) -> bool {
    git(work_dir, &["merge-base", "--is-ancestor", a, b])
        .await
        .is_ok()
}

/// Merge-base of `HEAD` with the branch's base, choosing between the local
/// `<base>` ref and its `origin/<base>` tracking counterpart whichever sits
/// *closer to* `HEAD`.
///
/// A weaver branch is forked from the local `<base>` at creation, but that local
/// ref then often falls behind `origin/<base>` (nobody checks `main` out to pull
/// it) while the branch itself gets rebased onto the remote. Diffing against the
/// stale local ref would then replay every intervening upstream commit as a
/// spurious change — the "cruft from unrelated upstream changes" a long-lived
/// branch accumulates. Taking the more recent of the two merge-bases keeps the
/// diff to the branch's own work whichever ref it actually tracks.
pub async fn merge_base_fresh(work_dir: &Path, base: &str) -> Result<String> {
    let remote = format!("origin/{base}");
    let mut best: Option<String> = None;
    for cand in [base, remote.as_str()] {
        let Ok(mb) = git(work_dir, &["merge-base", cand, "HEAD"]).await else {
            continue; // ref doesn't resolve (no such branch / no remote) — skip it
        };
        best = Some(match best {
            None => mb,
            // Prefer the merge-base nearer HEAD: if the previous pick is an
            // ancestor of this one, this one is the more recent fork point.
            Some(prev) if is_ancestor(work_dir, &prev, &mb).await => mb,
            Some(prev) => prev,
        });
    }
    best.ok_or_else(|| anyhow!("no merge-base between HEAD and {base} or origin/{base}"))
}

/// Which baseline a worktree diff is taken against. The string values are the
/// wire form carried by the file-viewer's `base` query parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffBase {
    /// The branch's fork point — everything the branch introduced. The default.
    Branch,
    /// `HEAD` — only changes not yet committed.
    Uncommitted,
}

impl DiffBase {
    /// Parse the `base` query value; anything unrecognized (or absent) is `Branch`.
    pub fn from_query(s: Option<&str>) -> Self {
        match s {
            Some("uncommitted") => DiffBase::Uncommitted,
            _ => DiffBase::Branch,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DiffBase::Branch => "branch",
            DiffBase::Uncommitted => "uncommitted",
        }
    }
}

/// Resolve the git revision a worktree diff should compare against for `mode`:
/// the freshest merge-base with `base` for [`DiffBase::Branch`], or plain `HEAD`
/// for [`DiffBase::Uncommitted`]. Feed the result to [`changed_files`],
/// [`diff`], or [`read_blob`] as `since`.
pub async fn diff_since(work_dir: &Path, base: &str, mode: DiffBase) -> Result<String> {
    match mode {
        DiffBase::Branch => merge_base_fresh(work_dir, base).await,
        DiffBase::Uncommitted => Ok("HEAD".to_string()),
    }
}

async fn git_with_index(work_dir: &Path, index: &Path, args: &[&str]) -> Result<String> {
    tracing::debug!(
        ?args,
        dir = %work_dir.display(),
        index = %index.display(),
        "running git (temp index)"
    );
    let out = Command::new("git")
        .arg("-C")
        .arg(work_dir)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .output()
        .await
        .context("failed to spawn git")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        tracing::warn!(
            args = %args.join(" "),
            dir = %work_dir.display(),
            code = out.status.code().unwrap_or(-1),
            stderr = %truncate(stderr.trim(), 500),
            stdout = %truncate(stdout.trim(), 500),
            "git failed (temp index)"
        );
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Copy the worktree's real index to a throwaway file so a diff can `git add`
/// into it (capturing untracked files) without disturbing the real index.
async fn temp_index(work_dir: &Path) -> Result<PathBuf> {
    let rel = git(work_dir, &["rev-parse", "--git-path", "index"]).await?;
    let src = {
        let p = PathBuf::from(&rel);
        if p.is_absolute() {
            p
        } else {
            work_dir.join(p)
        }
    };
    let unique = format!(
        "weaver-index-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let dst = std::env::temp_dir().join(unique);
    if tokio::fs::try_exists(&src).await.unwrap_or(false) {
        tokio::fs::copy(&src, &dst)
            .await
            .context("copying git index")?;
    }
    Ok(dst)
}

// Pathspec that keeps weaver's own injected files out of diffs/summaries.
const DIFF_PATHSPEC: &[&str] = &[".", ":(exclude).claude"];

/// Diff `args` (`["diff", "--cached", since, ...]`) against a throwaway index
/// that has every change — including untracked files — staged into it.
async fn inclusive_diff(work_dir: &Path, args: &[&str]) -> Result<String> {
    let index = temp_index(work_dir).await?;
    let _ = git_with_index(work_dir, &index, &["add", "-A"]).await;
    let result = git_with_index(work_dir, &index, args).await;
    let _ = tokio::fs::remove_file(&index).await;
    result
}

/// Full unified diff of everything (committed + uncommitted + untracked) since `since`.
pub async fn diff(work_dir: &Path, since: &str) -> Result<String> {
    let mut args = vec!["diff", "--cached", since, "--"];
    args.extend_from_slice(DIFF_PATHSPEC);
    let out = inclusive_diff(work_dir, &args).await?;
    tracing::debug!(
        dir = %work_dir.display(),
        since,
        diff_chars = out.len(),
        "computed diff"
    );
    Ok(out)
}

/// Aggregate diff stats since `since`, including untracked files.
pub async fn diff_stat(work_dir: &Path, since: &str) -> Result<DiffStat> {
    let mut args = vec!["diff", "--cached", "--numstat", since, "--"];
    args.extend_from_slice(DIFF_PATHSPEC);
    let out = inclusive_diff(work_dir, &args).await?;
    let mut stat = DiffStat::default();
    for line in out.lines() {
        let mut parts = line.split('\t');
        let added = parts.next().unwrap_or("-");
        let removed = parts.next().unwrap_or("-");
        stat.files_changed += 1;
        stat.insertions += added.parse::<i64>().unwrap_or(0);
        stat.deletions += removed.parse::<i64>().unwrap_or(0);
    }
    Ok(stat)
}

// ---------------------------------------------------------------------------
// Worktree browsing — file listing, change detection, and blob reads, used by
// loom's file-viewer endpoints.
// ---------------------------------------------------------------------------

/// One changed file relative to a base ref, with its change status.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    /// One of `added`, `modified`, `deleted`, `renamed`, `copied`.
    pub status: String,
}

/// Every file git considers part of the worktree: tracked files plus untracked
/// files that are **not** gitignored (so `target/`, `node_modules/`, and the
/// `scratch/` drop directory are skipped). Paths are repo-relative,
/// `/`-separated, sorted and de-duplicated. `-z` keeps names with spaces or
/// unicode intact.
pub async fn list_files(work_dir: &Path) -> Result<Vec<String>> {
    let out = git(
        work_dir,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )
    .await?;
    let mut files: Vec<String> = out
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// Files changed since `since`, including still-untracked ones (staged into a
/// throwaway index, the same trick [`diff`] uses). Renames/copies report the
/// destination path. weaver's own `.claude` injections are excluded.
pub async fn changed_files(work_dir: &Path, since: &str) -> Result<Vec<ChangedFile>> {
    let mut args = vec!["diff", "--cached", "--name-status", "-z", since, "--"];
    args.extend_from_slice(DIFF_PATHSPEC);
    let out = inclusive_diff(work_dir, &args).await?;
    // `-z --name-status` emits NUL-separated fields: `<status>\0<path>\0`, except
    // renames/copies which carry two paths: `<status>\0<old>\0<new>\0`.
    let mut fields = out.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(code) = fields.next() {
        let letter = code.chars().next().unwrap_or('?');
        // R/C carry an old path then a new path; keep the destination.
        let path = if letter == 'R' || letter == 'C' {
            fields.next();
            fields.next()
        } else {
            fields.next()
        };
        let Some(path) = path else { break };
        let status = match letter {
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            'C' => "copied",
            _ => "modified", // M, T (type change), and anything unexpected
        };
        files.push(ChangedFile {
            path: path.to_string(),
            status: status.to_string(),
        });
    }
    Ok(files)
}

/// Raw bytes of `path` at revision `rev` (e.g. a merge-base sha). Returns `None`
/// when the path does not exist at that revision — i.e. a file the branch added,
/// which the file viewer renders as an empty original side of the diff.
pub async fn read_blob(work_dir: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("{rev}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(work_dir)
        .args(["show", &spec])
        .output()
        .await
        .context("failed to spawn git show")?;
    if out.status.success() {
        Ok(Some(out.stdout))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env-var-borne `http.extraheader` decodes to exactly
    /// `x-access-token:<token>` — the Basic-auth shape a GitHub App
    /// installation token expects — and nothing about it (an `Authorization`
    /// header, a config key) leaks into a persisted git config, since these are
    /// only ever passed as subprocess env, never written to `.git/config`.
    #[test]
    fn token_auth_envs_carries_a_correctly_shaped_extraheader() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let envs = token_auth_envs("ghs_abc123");
        let pairs: std::collections::HashMap<_, _> = envs.into_iter().collect();
        assert_eq!(pairs["GIT_CONFIG_COUNT"], "1");
        assert_eq!(pairs["GIT_CONFIG_KEY_0"], "http.extraheader");
        let header = &pairs["GIT_CONFIG_VALUE_0"];
        let encoded = header
            .strip_prefix("AUTHORIZATION: basic ")
            .expect("header should be a Basic auth line");
        let decoded = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(decoded, "x-access-token:ghs_abc123");
    }

    async fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).await.unwrap()
    }

    async fn commit(dir: &Path, msg: &str) -> String {
        run(dir, &["commit", "--allow-empty", "-q", "-m", msg]).await;
        run(dir, &["rev-parse", "HEAD"]).await
    }

    /// A stale local `main` left behind at the fork point must not drag the
    /// upstream commits the branch was rebased onto into its diff. The freshest
    /// merge-base (here `origin/main`) keeps the diff to the branch's own work.
    #[tokio::test]
    async fn merge_base_fresh_prefers_up_to_date_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        run(dir, &["init", "-q", "-b", "main"]).await;
        run(dir, &["config", "user.email", "t@t.t"]).await;
        run(dir, &["config", "user.name", "t"]).await;

        let t0 = commit(dir, "T0").await;
        // Two upstream commits the branch will be rebased onto.
        std::fs::write(dir.join("SKILL.md"), "s\n").unwrap();
        run(dir, &["add", "-A"]).await;
        commit(dir, "U1 upstream").await;
        std::fs::write(dir.join("dependabot.yml"), "d\n").unwrap();
        run(dir, &["add", "-A"]).await;
        let upstream = commit(dir, "U2 upstream").await;
        // origin/main tracks the up-to-date upstream tip.
        run(dir, &["update-ref", "refs/remotes/origin/main", &upstream]).await;

        // The branch forks from the up-to-date remote and adds one real change.
        run(dir, &["checkout", "-q", "-b", "feature", &upstream]).await;
        std::fs::write(dir.join("mine.txt"), "m\n").unwrap();
        run(dir, &["add", "-A"]).await;
        commit(dir, "my real change").await;

        // Local `main` is left stale at the original fork point.
        run(dir, &["update-ref", "refs/heads/main", &t0]).await;

        // Plain merge-base against the stale local ref is the bug: it predates
        // the upstream commits, so the diff replays them as cruft.
        assert_eq!(merge_base(dir, "main").await.unwrap(), t0);
        let crufty = changed_files(dir, &t0).await.unwrap();
        let names: Vec<&str> = crufty.iter().map(|c| c.path.as_str()).collect();
        assert!(names.contains(&"SKILL.md") && names.contains(&"dependabot.yml"));

        // The fresh resolution snaps to origin/main, dropping the upstream churn.
        let since = diff_since(dir, "main", DiffBase::Branch).await.unwrap();
        assert_eq!(since, upstream, "should diff against the up-to-date remote");
        let clean = changed_files(dir, &since).await.unwrap();
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].path, "mine.txt");
    }

    /// `clone` acquires a repo into a fresh dest, then is idempotent: a second
    /// call fetches into the existing clone (picking up new commits) instead of
    /// failing on the populated directory. Uses a local bare repo as the remote —
    /// no network.
    #[tokio::test]
    async fn clone_then_fetch_is_idempotent() {
        // A working source repo with one commit.
        let src = tempfile::tempdir().unwrap();
        let sdir = src.path();
        run(sdir, &["init", "-q", "-b", "main"]).await;
        run(sdir, &["config", "user.email", "t@t.t"]).await;
        run(sdir, &["config", "user.name", "t"]).await;
        std::fs::write(sdir.join("a.txt"), "a\n").unwrap();
        run(sdir, &["add", "-A"]).await;
        commit(sdir, "init").await;

        // A bare clone of it acts as the clonable remote.
        let bare = tempfile::tempdir().unwrap();
        let bare_path = bare.path().join("origin.git");
        let out = Command::new("git")
            .args([
                "clone",
                "--bare",
                "-q",
                &sdir.to_string_lossy(),
                &bare_path.to_string_lossy(),
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "bare clone of the source failed");
        let bare_url = bare_path.to_string_lossy().to_string();

        // First call clones into a nested <root>/owner/name path (parent created).
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("acme").join("widgets");
        clone(&bare_url, &dest, None).await.unwrap();
        assert!(dest.join(".git").exists(), "clone made a working clone");
        assert!(dest.join("a.txt").exists(), "clone checked out content");

        // Advance the remote with a second commit.
        std::fs::write(sdir.join("b.txt"), "b\n").unwrap();
        run(sdir, &["add", "-A"]).await;
        let second = commit(sdir, "second").await;
        run(sdir, &["push", "-q", &bare_url, "main"]).await;

        // Idempotent: the second call fetches rather than erroring, and the new
        // commit is now reachable via origin.
        clone(&bare_url, &dest, None).await.unwrap();
        assert!(
            commit_exists(&dest, &second).await,
            "fetch pulled the new remote commit into the existing clone"
        );
    }

    /// With no `origin` remote, the launch base degrades to the local current
    /// branch — the historical behaviour, kept for remote-less repos.
    #[tokio::test]
    async fn default_base_without_remote_uses_current_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        run(dir, &["init", "-q", "-b", "main"]).await;
        run(dir, &["config", "user.email", "t@t.t"]).await;
        run(dir, &["config", "user.name", "t"]).await;
        commit(dir, "T0").await;
        assert_eq!(default_base(dir).await.unwrap(), "main");
    }

    /// With an `origin` remote, the launch base is the remote's default branch
    /// (`origin/main`) so new work forks from the fetched mainline.
    #[tokio::test]
    async fn default_base_prefers_origin_default() {
        let remote = tempfile::tempdir().unwrap();
        run(remote.path(), &["init", "-q", "--bare", "-b", "main"]).await;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        run(dir, &["init", "-q", "-b", "main"]).await;
        run(dir, &["config", "user.email", "t@t.t"]).await;
        run(dir, &["config", "user.name", "t"]).await;
        commit(dir, "T0").await;
        let remote_url = remote.path().to_string_lossy().to_string();
        run(dir, &["remote", "add", "origin", &remote_url]).await;
        run(dir, &["push", "-q", "origin", "main"]).await;
        // Populate the remote-tracking refs (origin/main, origin/HEAD).
        run(dir, &["fetch", "-q", "origin"]).await;
        run(dir, &["remote", "set-head", "origin", "main"]).await;

        assert_eq!(default_base(dir).await.unwrap(), "origin/main");
    }

    /// `DiffBase::Uncommitted` scopes to working-tree changes vs `HEAD`, ignoring
    /// everything the branch has already committed.
    #[tokio::test]
    async fn diff_since_uncommitted_is_vs_head() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        run(dir, &["init", "-q", "-b", "main"]).await;
        run(dir, &["config", "user.email", "t@t.t"]).await;
        run(dir, &["config", "user.name", "t"]).await;
        commit(dir, "T0").await;
        std::fs::write(dir.join("committed.txt"), "c\n").unwrap();
        run(dir, &["add", "-A"]).await;
        commit(dir, "committed work").await;

        // An as-yet-uncommitted edit.
        std::fs::write(dir.join("scratchpad.txt"), "wip\n").unwrap();

        assert_eq!(
            diff_since(dir, "main", DiffBase::Uncommitted)
                .await
                .unwrap(),
            "HEAD"
        );
        let changed = changed_files(dir, "HEAD").await.unwrap();
        assert_eq!(changed.len(), 1, "only the uncommitted file");
        assert_eq!(changed[0].path, "scratchpad.txt");
    }
}
