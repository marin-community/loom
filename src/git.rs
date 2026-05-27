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
    tracing::debug!(?args, dir = %dir.display(), "running git");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
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
    let current = tokio::fs::read_to_string(&exclude).await.unwrap_or_default();
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
    Ok(())
}

pub async fn current_branch(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await
}

pub async fn branch_exists(dir: &Path, branch: &str) -> bool {
    git(
        dir,
        &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
    )
    .await
    .is_ok()
}

/// Create a new worktree at `path` on a new `branch` forked from `base`.
pub async fn worktree_add(repo_root: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    let path = path.to_string_lossy();
    git(
        repo_root,
        &["worktree", "add", "-b", branch, &path, base],
    )
    .await?;
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

pub async fn delete_branch(repo_root: &Path, branch: &str) -> Result<()> {
    git(repo_root, &["branch", "-D", branch]).await?;
    tracing::info!(%branch, "branch deleted");
    Ok(())
}

/// Merge-base SHA between `base` and the worktree's `HEAD`.
pub async fn merge_base(work_dir: &Path, base: &str) -> Result<String> {
    git(work_dir, &["merge-base", base, "HEAD"]).await
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
        tokio::fs::copy(&src, &dst).await.context("copying git index")?;
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

/// True when the working tree has no uncommitted changes.
pub async fn is_clean(dir: &Path) -> Result<bool> {
    Ok(git(dir, &["status", "--porcelain"]).await?.is_empty())
}

/// Merge `branch` into the current branch of `repo_root` (no fast-forward).
pub async fn merge(repo_root: &Path, branch: &str) -> Result<String> {
    let out = git(repo_root, &["merge", "--no-ff", branch]).await?;
    tracing::info!(%branch, repo = %repo_root.display(), "branch merged");
    Ok(out)
}
