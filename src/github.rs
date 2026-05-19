//! Optional GitHub integration via the `gh` CLI. All functions degrade
//! gracefully — callers treat errors as "GitHub unavailable".

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: String,
}

async fn gh(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .context("failed to spawn gh (is the GitHub CLI installed?)")?;
    if !out.status.success() {
        bail!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `owner/name` slug for the repository at `repo_root`.
pub async fn repo_slug(repo_root: &Path) -> Result<String> {
    gh(
        repo_root,
        &["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"],
    )
    .await
}

/// Fetch an issue's title/body/url.
pub async fn fetch_issue(repo_root: &Path, number: i64) -> Result<Issue> {
    let json = gh(
        repo_root,
        &[
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "title,body,url",
        ],
    )
    .await?;
    serde_json::from_str(&json).context("parsing gh issue JSON")
}

/// Open a pull request from the workspace branch; returns the PR URL.
pub async fn create_pr(work_dir: &Path, base: &str, title: &str, body: &str) -> Result<String> {
    gh(
        work_dir,
        &[
            "pr", "create", "--base", base, "--title", title, "--body", body,
        ],
    )
    .await
}
