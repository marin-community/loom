//! The trusted-owner allowlist (`github_owners`): the GitHub accounts (orgs or
//! users) loom is authorized to act for via the inbound trigger.
//!
//! This exists to make loom's GitHub App safe to run **public**. GitHub only lets
//! a *private* App be installed on the account that owns it, so anyone wiring loom
//! up across an account boundary (a personal App onto an org's repos) is pushed to
//! flip the App public — at which point *anyone* can install it on their own
//! repos. Loom's "an installation is a grant" rule ([`crate::github_app::
//! GithubApp::ensure_installed_repo_registered`]) was only sound while the App was
//! private; once public it would auto-trust strangers. This allowlist re-anchors
//! that trust in an explicit set of owners rather than in "an installation
//! exists": a repo is auto-registered from its installation **only** when its
//! owner is listed here.
//!
//! The list is bootstrapped from `loom.toml` / the environment ([`crate::db`]'s
//! `seed_owners`) and extended by an operator through the web flow (`POST
//! /api/github/owners`). Logins are matched case-insensitively (the column is
//! `COLLATE NOCASE`), as GitHub logins are.

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::Row;

use crate::db::Db;

/// One trusted owner — a GitHub account login loom will act for.
#[derive(Debug, Clone, Serialize)]
pub struct Owner {
    pub login: String,
    pub created_at: String,
}

/// A GitHub login is `[A-Za-z0-9-]`, 1–39 chars, no leading/trailing hyphen. We
/// only enforce the charset and length (enough to keep junk out of the
/// allowlist); GitHub is the authority on whether the account exists.
pub fn valid_login(login: &str) -> bool {
    let login = login.trim();
    !login.is_empty()
        && login.len() <= 39
        && !login.starts_with('-')
        && !login.ends_with('-')
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Split a comma/whitespace-separated owner list (the `LOOM_ALLOWED_OWNERS`
/// form) into individual trimmed, non-empty logins.
pub fn split_logins(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', ' ', '\t', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Every trusted owner, newest first.
pub async fn list(db: &Db) -> Result<Vec<Owner>> {
    let rows =
        sqlx::query("SELECT login, created_at FROM github_owners ORDER BY created_at DESC, login")
            .fetch_all(db)
            .await
            .context("listing trusted owners")?;
    Ok(rows
        .into_iter()
        .map(|r| Owner {
            login: r.get("login"),
            created_at: r.get("created_at"),
        })
        .collect())
}

/// Whether `login` is a trusted owner (case-insensitive). An empty/malformed
/// login is never trusted.
pub async fn is_allowed(db: &Db, login: &str) -> Result<bool> {
    let login = login.trim();
    if login.is_empty() {
        return Ok(false);
    }
    let row = sqlx::query("SELECT 1 AS one FROM github_owners WHERE login = ?")
        .bind(login)
        .fetch_optional(db)
        .await
        .context("checking the trusted-owner allowlist")?;
    Ok(row.is_some())
}

/// Add a trusted owner. Returns the stored row. Idempotent-ish: adding an
/// existing login (any case) is an error from the caller's view only if you
/// need the insert to be new; here we upsert-by-ignore then read it back.
pub async fn add(db: &Db, login: &str) -> Result<Owner> {
    let login = login.trim();
    if !valid_login(login) {
        anyhow::bail!("'{login}' is not a valid GitHub login");
    }
    sqlx::query("INSERT OR IGNORE INTO github_owners (login) VALUES (?)")
        .bind(login)
        .execute(db)
        .await
        .with_context(|| format!("adding trusted owner '{login}'"))?;
    get(db, login)
        .await?
        .with_context(|| format!("trusted owner '{login}' vanished after insert"))
}

/// Read one owner back by login (case-insensitive), if present.
pub async fn get(db: &Db, login: &str) -> Result<Option<Owner>> {
    let row = sqlx::query("SELECT login, created_at FROM github_owners WHERE login = ?")
        .bind(login.trim())
        .fetch_optional(db)
        .await
        .context("reading a trusted owner")?;
    Ok(row.map(|r| Owner {
        login: r.get("login"),
        created_at: r.get("created_at"),
    }))
}

/// Remove a trusted owner. Returns whether a row was deleted.
pub async fn remove(db: &Db, login: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM github_owners WHERE login = ?")
        .bind(login.trim())
        .execute(db)
        .await
        .with_context(|| format!("removing trusted owner '{login}'"))?;
    Ok(res.rows_affected() > 0)
}

/// Seed one login into the allowlist (`INSERT OR IGNORE`), skipping anything
/// malformed so a stray value in the bootstrap list can't fail startup.
pub async fn seed(db: &Db, login: &str) -> Result<()> {
    let login = login.trim();
    if !valid_login(login) {
        tracing::warn!(login, "ignoring malformed bootstrap owner login");
        return Ok(());
    }
    sqlx::query("INSERT OR IGNORE INTO github_owners (login) VALUES (?)")
        .bind(login)
        .execute(db)
        .await
        .with_context(|| format!("seeding trusted owner '{login}'"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        crate::db::connect_in_memory().await.unwrap()
    }

    #[test]
    fn valid_login_rules() {
        assert!(valid_login("acme"));
        assert!(valid_login("My-Org-1"));
        assert!(!valid_login(""));
        assert!(!valid_login("-lead"));
        assert!(!valid_login("trail-"));
        assert!(!valid_login("has space"));
        assert!(!valid_login("bad/slug"));
    }

    #[test]
    fn split_logins_splits_on_comma_and_space() {
        let got: Vec<&str> = split_logins(" acme, widgets  co\nother ").collect();
        assert_eq!(got, vec!["acme", "widgets", "co", "other"]);
    }

    #[tokio::test]
    async fn add_list_remove_roundtrip() {
        let db = db().await;
        add(&db, "acme").await.unwrap();
        add(&db, "widgets").await.unwrap();
        let logins: Vec<String> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|o| o.login)
            .collect();
        assert!(logins.contains(&"acme".to_string()));
        assert!(logins.contains(&"widgets".to_string()));
        assert!(remove(&db, "acme").await.unwrap());
        assert!(!remove(&db, "acme").await.unwrap());
    }

    #[tokio::test]
    async fn is_allowed_is_case_insensitive() {
        let db = db().await;
        add(&db, "Acme").await.unwrap();
        assert!(is_allowed(&db, "acme").await.unwrap());
        assert!(is_allowed(&db, "ACME").await.unwrap());
        assert!(!is_allowed(&db, "widgets").await.unwrap());
        assert!(!is_allowed(&db, "").await.unwrap());
        // A second add differing only in case does not create a duplicate row.
        add(&db, "acme").await.unwrap();
        let acme_rows = list(&db)
            .await
            .unwrap()
            .into_iter()
            .filter(|o| o.login.eq_ignore_ascii_case("acme"))
            .count();
        assert_eq!(acme_rows, 1);
    }

    #[tokio::test]
    async fn seed_ignores_malformed() {
        let db = db().await;
        seed(&db, "good-org").await.unwrap();
        seed(&db, "bad slug").await.unwrap(); // ignored, not an error
        assert!(is_allowed(&db, "good-org").await.unwrap());
        assert!(!is_allowed(&db, "bad slug").await.unwrap());
    }
}
