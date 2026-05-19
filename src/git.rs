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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .context("failed to spawn git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
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
    Ok(())
}

pub async fn worktree_remove(repo_root: &Path, path: &Path) -> Result<()> {
    let path = path.to_string_lossy();
    git(repo_root, &["worktree", "remove", "--force", &path]).await?;
    Ok(())
}

pub async fn delete_branch(repo_root: &Path, branch: &str) -> Result<()> {
    git(repo_root, &["branch", "-D", branch]).await?;
    Ok(())
}

/// Merge-base SHA between `base` and the worktree's `HEAD`.
pub async fn merge_base(work_dir: &Path, base: &str) -> Result<String> {
    git(work_dir, &["merge-base", base, "HEAD"]).await
}

async fn git_with_index(work_dir: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(work_dir)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .output()
        .await
        .context("failed to spawn git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
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
    inclusive_diff(work_dir, &args).await
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
    git(repo_root, &["merge", "--no-ff", branch]).await
}
