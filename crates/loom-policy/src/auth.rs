//! Authentication for the loom daemon — who may drive the fleet over HTTP.
//!
//! This is a loom-only concern: the daemon-less `weaver` CLI talks straight to
//! sqlite and never authenticates. Three credential kinds all resolve to one
//! [`Principal`]:
//!
//! * **API tokens** (`loom_…`) — the `LOOM_TOKEN` a CI job or remote `loom` CLI
//!   sends as `Authorization: Bearer`. Stored hashed; shown once at creation.
//! * **Session cookies** — set after a GitHub or username/password login and
//!   carried by the browser. Stored hashed, same as tokens.
//! * **Loopback trust** — a request from `127.0.0.1`/`::1` is taken to be the
//!   machine owner (the seeded primary user), so the local CLI, the agent, and
//!   watch scripts keep working with zero configuration. Gated on the
//!   `auth.trust_loopback` setting (on by default).
//!
//! The machine also mints a **local token** ([`ensure_local_token`]) it injects
//! into its own subprocess environments, so same-host automation keeps working
//! even when loopback trust is turned off (the right posture behind a same-host
//! reverse proxy, where every forwarded request looks like loopback).
//!
//! This module is deliberately free of `axum` — it is the testable core
//! (crypto, the user/token/session tables, the GitHub OAuth calls). The HTTP
//! glue (the middleware, cookie headers, the route handlers) lives in
//! [`crate::web`].

use anyhow::{anyhow, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashMap;
use weaver_core::db::iso_in_days;

use crate::db::{now_iso, Db};

/// Prefix on every loom API token, so a leaked secret is recognisable and
/// greppable. A token looks like `loom_<43 url-safe base64 chars>`.
const TOKEN_PREFIX: &str = "loom_";
/// How much of a token's plaintext is kept (non-secret) for the token list —
/// enough to tell two tokens apart at a glance, far short of guessable.
const PREFIX_KEEP: usize = 12;
/// Browser login lifetime, in days. Shared by the stored-session expiry and the
/// `Max-Age` on the login cookie so the two can't drift.
pub const SESSION_TTL_DAYS: i64 = 30;
/// Maximum accepted stale GitHub organization membership after a successful
/// OAuth sign-in.
pub const GITHUB_ORGANIZATION_LEASE_HOURS: i64 = 1;
/// Revalidate shortly before the lease expires so a successful check extends
/// access without an avoidable gap.
pub const GITHUB_ORGANIZATION_REVALIDATION_LEAD_MINUTES: i64 = 1;
/// The cookie a browser login is carried in.
pub const SESSION_COOKIE: &str = "loom_session";
/// The reserved [`TokenKind::Local`] token name.
const LOCAL_TOKEN_NAME: &str = "this machine";

// Instants and expiries are computed app-side ([`now_iso`] / [`iso_in_days`])
// and bound as parameters — the stored ISO format orders lexicographically, so
// the `*_at` comparisons below need no SQL date functions.

// ---------------------------------------------------------------------------
// Principal
// ---------------------------------------------------------------------------

/// How a request proved its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthVia {
    /// Trusted because it came from the loopback interface.
    Loopback,
    /// A valid `Authorization: Bearer` API token.
    Token,
    /// A valid browser session cookie.
    Session,
    /// No credential was presented at all.
    ///
    /// Only reaches operations declaring `actor = Anonymous`; see
    /// [`Grant::Anonymous`].
    Nothing,
}

impl AuthVia {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthVia::Loopback => "loopback",
            AuthVia::Token => "token",
            AuthVia::Session => "session",
            AuthVia::Nothing => "none",
        }
    }
}

/// Capabilities carried by an authenticated identity. Admin is the compatibility
/// grant for users, browser sessions, loopback trust, PATs, and the local token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Grant {
    Admin,
    User,
    /// No credential was presented.
    ///
    /// Only operations declaring `actor = Anonymous` accept this, so a request
    /// with no principal yet is still authorized through the ordinary
    /// `authorize()` path.
    Anonymous,
    Automation {
        subject: String,
        profiles: Vec<String>,
    },
    Session {
        session_id: String,
        branch_id: String,
        /// `None` is an unrestricted session: actor and resource scope still
        /// apply, but newly registered session operations remain reachable.
        /// Restricted sessions carry the exact capability set derived from
        /// their immutable launch policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capabilities: Option<Vec<String>>,
    },
}

/// An authenticated caller: identity, proof mechanism, and explicit grant.
#[derive(Debug, Clone)]
pub struct Principal {
    pub username: String,
    pub github_login: Option<String>,
    pub via: AuthVia,
    pub grant: Grant,
    pub automation_context: Option<crate::automation::FederationContext>,
}

impl Principal {
    /// The caller that presented nothing.
    ///
    /// Constructed by the auth middleware only for requests whose target
    /// operation declares `actor = Anonymous`. See [`Grant::Anonymous`].
    pub fn anonymous() -> Self {
        Self {
            username: String::new(),
            github_login: None,
            via: AuthVia::Nothing,
            grant: Grant::Anonymous,
            automation_context: None,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self.grant, Grant::Admin)
    }

    pub fn is_human(&self) -> bool {
        matches!(self.grant, Grant::Admin | Grant::User)
    }

    pub fn user_role(&self) -> Option<UserRole> {
        match self.grant {
            Grant::Admin => Some(UserRole::Admin),
            Grant::User => Some(UserRole::User),
            Grant::Automation { .. } | Grant::Session { .. } | Grant::Anonymous => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Crypto primitives
// ---------------------------------------------------------------------------

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

/// `bytes` cryptographically-random bytes as url-safe base64 (no padding).
fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// A short random hex id (token / row identifier).
fn random_id() -> String {
    let mut buf = [0u8; 8];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// A short random state nonce for the OAuth round-trip (CSRF guard).
pub fn random_state() -> String {
    random_b64(18)
}

/// Mint a fresh secret token: `(plaintext, sha256-hash, display-prefix)`. Only
/// the hash and prefix are persisted; the plaintext is returned to the caller
/// once and never stored.
fn mint_token() -> (String, String, String) {
    let plain = format!("{TOKEN_PREFIX}{}", random_b64(32));
    let hash = sha256_hex(&plain);
    let prefix: String = plain.chars().take(PREFIX_KEEP).collect();
    (plain, hash, prefix)
}

/// Hash a password for storage with argon2id (per-password random salt). The
/// salt is drawn from the same CSPRNG as our tokens, then b64-encoded into the
/// PHC salt string — sidestepping argon2's `rand_core` version pin.
pub fn hash_password(password: &str) -> Result<String> {
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| anyhow!("encoding salt: {e}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("hashing password: {e}"))
}

/// Constant-time verify a password against a stored argon2 hash. A malformed
/// stored hash fails (never panics).
fn verify_password(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Users (the approved-operator allowlist)
// ---------------------------------------------------------------------------

/// A persisted human role. Existing rows migrate to `admin`; newly approved
/// people default to `user`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    #[default]
    User,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }

    fn grant(self) -> Grant {
        match self {
            Self::Admin => Grant::Admin,
            Self::User => Grant::User,
        }
    }
}

/// Where a user's current Loom authorization comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum UserAuthorizationKind {
    #[default]
    Manual,
    GithubOrganization,
}

impl UserAuthorizationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::GithubOrganization => "github_organization",
        }
    }
}

/// One approved Loom user.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub username: String,
    pub github_login: Option<String>,
    pub github_user_id: Option<i64>,
    pub password_hash: Option<String>,
    pub role: UserRole,
    pub authorization_kind: UserAuthorizationKind,
    pub authorization_github_org_id: Option<i64>,
    pub authorization_github_org_login: Option<String>,
    pub authorization_valid_until: Option<String>,
    pub created_at: String,
}

/// The durable identity and source of an organization-derived authorization
/// that can be revalidated with the GitHub App.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GithubOrganizationAuthorization {
    pub username: String,
    pub github_user_id: i64,
    pub organization_id: i64,
    pub valid_until: String,
}

fn github_organization_authorization_from_user(
    user: &User,
    github_user_id: i64,
) -> Result<Option<GithubOrganizationAuthorization>> {
    if user.is_manually_authorized() {
        return Ok(None);
    }
    Ok(Some(GithubOrganizationAuthorization {
        username: user.username.clone(),
        github_user_id,
        organization_id: user
            .authorization_github_org_id
            .ok_or_else(|| anyhow!("organization authorization has no organization id"))?,
        valid_until: user
            .authorization_valid_until
            .clone()
            .ok_or_else(|| anyhow!("organization authorization has no deadline"))?,
    }))
}

impl User {
    /// Whether this user can log in with a password (has one set).
    pub fn has_password(&self) -> bool {
        self.password_hash.is_some()
    }

    pub fn is_manually_authorized(&self) -> bool {
        self.authorization_kind == UserAuthorizationKind::Manual
    }

    fn is_authorized_at(&self, now: &str) -> bool {
        self.is_manually_authorized()
            || self
                .authorization_valid_until
                .as_deref()
                .is_some_and(|valid_until| valid_until > now)
    }
}

pub async fn get_user(db: &Db, username: &str) -> Result<Option<User>> {
    let row = sqlx::query_as::<_, User>(
        "SELECT username, github_login, github_user_id, password_hash, role, authorization_kind,
                authorization_github_org_id, authorization_github_org_login,
                authorization_valid_until, created_at
         FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// The user whose display `github_login` matches case-insensitively. OAuth
/// authorization uses only the numeric GitHub id; login-only lookup remains
/// for display-oriented administration and explicit migration errors.
pub async fn user_by_github(db: &Db, login: &str) -> Result<Option<User>> {
    let row = sqlx::query_as::<_, User>(
        "SELECT username, github_login, github_user_id, password_hash, role, authorization_kind,
                authorization_github_org_id, authorization_github_org_login,
                authorization_valid_until, created_at FROM users
         WHERE github_login IS NOT NULL AND lower(github_login) = lower(?)",
    )
    .bind(login)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// A GitHub identity whose immutable id matches and whose manual grant or
/// organization lease is currently valid. A signed webhook's current login is
/// display data, so it is refreshed only after the durable id matches.
pub async fn authorized_github_identity(
    db: &Db,
    login: &str,
    github_user_id: i64,
) -> Result<Option<User>> {
    let updated = sqlx::query(
        "UPDATE users SET github_login = ?
         WHERE github_user_id = ?
           AND (authorization_kind = 'manual' OR authorization_valid_until > ?)",
    )
    .bind(login)
    .bind(github_user_id)
    .bind(now_iso())
    .execute(db)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(None);
    }
    sqlx::query_as::<_, User>(
        "SELECT username, github_login, github_user_id, password_hash, role, authorization_kind,
                authorization_github_org_id, authorization_github_org_login,
                authorization_valid_until, created_at FROM users
         WHERE github_user_id = ?
           AND (authorization_kind = 'manual' OR authorization_valid_until > ?)",
    )
    .bind(github_user_id)
    .bind(now_iso())
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

/// Organization-derived authorization for a durable GitHub identity,
/// including an expired lease. Used for point-of-use revalidation of webhook
/// actors.
pub async fn github_organization_authorization_for_identity(
    db: &Db,
    github_user_id: i64,
) -> Result<Option<GithubOrganizationAuthorization>> {
    sqlx::query_as(
        "SELECT username, github_user_id,
                authorization_github_org_id AS organization_id,
                authorization_valid_until AS valid_until
         FROM users
         WHERE github_user_id = ? AND authorization_kind = 'github_organization'",
    )
    .bind(github_user_id)
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

/// Organization authorizations whose one-hour lease is about to expire.
pub async fn github_organization_authorizations_due(
    db: &Db,
) -> Result<Vec<GithubOrganizationAuthorization>> {
    let revalidate_before = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(
            GITHUB_ORGANIZATION_REVALIDATION_LEAD_MINUTES,
        ))
        .ok_or_else(|| anyhow!("GitHub organization revalidation deadline overflowed"))?
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    sqlx::query_as(
        "SELECT username, github_user_id,
                authorization_github_org_id AS organization_id,
                authorization_valid_until AS valid_until
         FROM users
         WHERE authorization_kind = 'github_organization'
           AND authorization_valid_until <= ?",
    )
    .bind(revalidate_before)
    .fetch_all(db)
    .await
    .map_err(Into::into)
}

/// Extend a still-current organization authorization after GitHub confirms
/// membership. The previous deadline is part of the compare-and-swap so a
/// stale check cannot overwrite a newer sign-in or an administrator's manual
/// conversion.
pub async fn renew_github_organization_authorization(
    db: &Db,
    authorization: &GithubOrganizationAuthorization,
    github_login: &str,
    organization: &crate::config::GithubOrganization,
) -> Result<bool> {
    let valid_until = github_organization_valid_until()?;
    let updated = sqlx::query(
        "UPDATE users
         SET github_login = ?, role = 'user', authorization_github_org_id = ?,
             authorization_github_org_login = ?, authorization_valid_until = ?
         WHERE username = ? AND github_user_id = ?
           AND authorization_kind = 'github_organization'
           AND authorization_github_org_id = ?
           AND authorization_valid_until = ?",
    )
    .bind(github_login)
    .bind(organization.id)
    .bind(&organization.login)
    .bind(valid_until)
    .bind(&authorization.username)
    .bind(authorization.github_user_id)
    .bind(authorization.organization_id)
    .bind(&authorization.valid_until)
    .execute(db)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// Revoke a still-current organization authorization after an inactive or
/// indeterminate GitHub result. Returns false when a concurrent sign-in or
/// manual conversion already changed the grant.
pub async fn expire_github_organization_authorization(
    db: &Db,
    authorization: &GithubOrganizationAuthorization,
) -> Result<bool> {
    let updated = sqlx::query(
        "UPDATE users SET authorization_valid_until = ?
         WHERE username = ? AND github_user_id = ?
           AND authorization_kind = 'github_organization'
           AND authorization_github_org_id = ?
           AND authorization_valid_until = ?",
    )
    .bind(now_iso())
    .bind(&authorization.username)
    .bind(authorization.github_user_id)
    .bind(authorization.organization_id)
    .bind(&authorization.valid_until)
    .execute(db)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// The derived username for a GitHub identity whose authorization lease is
/// expired. Used to close the sessions it owns after failed revalidation.
pub async fn expired_github_organization_username(
    db: &Db,
    github_user_id: i64,
) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT username FROM users
         WHERE github_user_id = ? AND authorization_kind = 'github_organization'
           AND authorization_valid_until <= ?",
    )
    .bind(github_user_id)
    .bind(now_iso())
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

/// The git author/committer identity to attribute a user's commits to.
#[derive(Debug, Clone)]
pub struct CommitIdentity {
    pub name: String,
    pub email: String,
}

/// The commit identity for `username`, or `None` if the user has no GitHub
/// login on file (a password-only operator — nothing to attribute to). The
/// email is GitHub's stable `<id>+<login>@users.noreply.github.com` form, which
/// links the commit to the account without exposing a private address; it falls
/// back to the id-less `<login>@…` form for a legacy manual user who has not
/// completed GitHub sign-in since durable identity binding was introduced.
/// The name is the captured display name, else the login.
pub async fn commit_identity(db: &Db, username: &str) -> Result<Option<CommitIdentity>> {
    let Some(row) = sqlx::query(
        "SELECT github_login, github_user_id, display_name FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    let Some(login) = row.get::<Option<String>, _>("github_login") else {
        return Ok(None);
    };
    let id = row.get::<Option<i64>, _>("github_user_id");
    let display = row.get::<Option<String>, _>("display_name");
    let email = match id {
        Some(id) => format!("{id}+{login}@users.noreply.github.com"),
        None => format!("{login}@users.noreply.github.com"),
    };
    let name = display.filter(|s| !s.trim().is_empty()).unwrap_or(login);
    Ok(Some(CommitIdentity { name, email }))
}

pub async fn list_users(db: &Db) -> Result<Vec<User>> {
    let rows = sqlx::query_as::<_, User>(
        "SELECT username, github_login, github_user_id, password_hash, role, authorization_kind,
                authorization_github_org_id, authorization_github_org_login,
                authorization_valid_until, created_at
         FROM users ORDER BY created_at, username",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The primary (owner) user: the earliest-created row. Loopback requests and the
/// machine token are attributed to them. `None` only on an unseeded database.
pub async fn primary_user(db: &Db) -> Result<Option<String>> {
    let row = sqlx::query(
        "SELECT username FROM users WHERE authorization_kind = 'manual'
         ORDER BY created_at, username LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| r.get::<String, _>("username")))
}

/// Add an approved user. `github_login` and `github_user_id` together enable
/// GitHub login; `password` enables password login. At least one method should
/// be set or the user can never authenticate, but that is the caller's policy.
pub async fn add_user(
    db: &Db,
    username: &str,
    github_login: Option<&str>,
    github_user_id: Option<i64>,
    password: Option<&str>,
    role: UserRole,
) -> Result<()> {
    if github_login.is_some() != github_user_id.is_some() {
        return Err(anyhow!(
            "a GitHub login and trusted numeric user id must be supplied together"
        ));
    }
    if github_user_id.is_some_and(|id| id <= 0) {
        return Err(anyhow!("GitHub user id must be positive"));
    }
    let password_hash = match password {
        Some(p) => Some(hash_password(p)?),
        None => None,
    };
    sqlx::query(
        "INSERT INTO users (username, github_login, github_user_id, password_hash, role)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(username)
    .bind(github_login)
    .bind(github_user_id)
    .bind(password_hash)
    .bind(role)
    .execute(db)
    .await
    .with_context(|| format!("adding user '{username}'"))?;
    Ok(())
}

/// Bind an administrator-confirmed immutable GitHub identity to a manual user.
/// OAuth never performs this transition from a mutable login on its own.
pub async fn set_manual_github_identity(
    db: &Db,
    username: &str,
    github_login: &str,
    github_user_id: i64,
) -> Result<()> {
    if !weaver_core::github::valid_login(github_login) || github_user_id <= 0 {
        return Err(anyhow!(
            "a valid GitHub login and positive numeric id are required"
        ));
    }
    let updated = sqlx::query(
        "UPDATE users SET github_login = ?, github_user_id = ?
         WHERE username = ? AND authorization_kind = 'manual'",
    )
    .bind(github_login)
    .bind(github_user_id)
    .bind(username)
    .execute(db)
    .await
    .with_context(|| format!("binding GitHub identity for '{username}'"))?;
    if updated.rows_affected() != 1 {
        return Err(anyhow!("no manually authorized user '{username}'"));
    }
    Ok(())
}

/// Change one user's human role without allowing the deployment to lose its
/// last administrator. Browser sessions and personal tokens resolve this row
/// on every request, so the new grant takes effect immediately.
pub async fn set_user_role(db: &Db, username: &str, role: UserRole) -> Result<()> {
    let mut tx = db.begin().await?;
    let current = sqlx::query("SELECT role, authorization_kind FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow!("no such user '{username}'"))?;
    let current_role = current.get::<UserRole, _>("role");
    let authorization_kind = current.get::<UserAuthorizationKind, _>("authorization_kind");
    if authorization_kind != UserAuthorizationKind::Manual {
        return Err(anyhow!(
            "organization-authorized users must be approved manually before their role can change"
        ));
    }
    if current_role == UserRole::Admin && role != UserRole::Admin {
        let admins =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE role = 'admin'")
                .fetch_one(&mut *tx)
                .await?;
        if admins <= 1 {
            return Err(anyhow!("cannot demote the only administrator"));
        }
    }
    sqlx::query("UPDATE users SET role = ? WHERE username = ?")
        .bind(role)
        .bind(username)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Permanently convert an organization-derived user to manual authorization.
/// The conditional update prevents an in-flight membership result from
/// overwriting an administrator's decision.
pub async fn approve_user_manually(db: &Db, username: &str) -> Result<User> {
    let updated = sqlx::query(
        "UPDATE users
         SET authorization_kind = 'manual',
             authorization_github_org_id = NULL,
             authorization_github_org_login = NULL,
             authorization_valid_until = NULL
         WHERE username = ? AND authorization_kind = 'github_organization'",
    )
    .bind(username)
    .execute(db)
    .await?;
    if updated.rows_affected() == 0 {
        let user = get_user(db, username)
            .await?
            .ok_or_else(|| anyhow!("no such user '{username}'"))?;
        if user.is_manually_authorized() {
            return Ok(user);
        }
        return Err(anyhow!("could not approve user '{username}' manually"));
    }
    get_user(db, username)
        .await?
        .ok_or_else(|| anyhow!("user '{username}' vanished after manual approval"))
}

pub async fn user_preferences(db: &Db, username: &str) -> Result<HashMap<String, String>> {
    let rows = sqlx::query("SELECT key, value FROM user_preferences WHERE username = ?")
        .bind(username)
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("key"), row.get("value")))
        .collect())
}

pub async fn apply_user_preferences(
    db: &Db,
    username: &str,
    changes: &[(String, Option<String>)],
) -> Result<()> {
    let mut tx = db.begin().await?;
    for (key, value) in changes {
        if let Some(value) = value {
            sqlx::query(
                "INSERT INTO user_preferences (username, key, value, updated_at)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(username, key) DO UPDATE SET
                   value = excluded.value,
                   updated_at = excluded.updated_at",
            )
            .bind(username)
            .bind(key)
            .bind(value)
            .bind(now_iso())
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query("DELETE FROM user_preferences WHERE username = ? AND key = ?")
                .bind(username)
                .bind(key)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Remove an approved user without allowing the deployment to lose its last
/// administrator. Returns whether a row was removed.
pub async fn remove_user(db: &Db, username: &str) -> Result<bool> {
    let mut tx = db.begin().await?;
    let Some(role) = sqlx::query_scalar::<_, UserRole>("SELECT role FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&mut *tx)
        .await?
    else {
        return Ok(false);
    };
    if role == UserRole::Admin {
        let admins =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE role = 'admin'")
                .fetch_one(&mut *tx)
                .await?;
        if admins <= 1 {
            return Err(anyhow!("cannot remove the only administrator"));
        }
    }
    let res = sqlx::query("DELETE FROM users WHERE username = ?")
        .bind(username)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(res.rows_affected() > 0)
}

/// Set (or, with `None`, clear) a user's password. Its tokens and sessions are
/// untouched.
pub async fn set_password(db: &Db, username: &str, password: Option<&str>) -> Result<()> {
    let hash = match password {
        Some(p) => Some(hash_password(p)?),
        None => None,
    };
    let res = sqlx::query(
        "UPDATE users SET password_hash = ?
         WHERE username = ? AND authorization_kind = 'manual'",
    )
    .bind(hash)
    .bind(username)
    .execute(db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(anyhow!(
            "no manually authorized user '{username}'; organization-authorized users cannot set passwords"
        ));
    }
    Ok(())
}

/// Verify a username/password login, returning the [`Principal`] on success.
/// A missing user, a user with no password, and a wrong password are all the
/// same indistinguishable failure (`Ok(None)`).
pub async fn verify_login(db: &Db, username: &str, password: &str) -> Result<Option<Principal>> {
    let Some(user) = get_user(db, username).await? else {
        return Ok(None);
    };
    if !user.is_manually_authorized() {
        return Ok(None);
    }
    let Some(stored) = user.password_hash.as_deref() else {
        return Ok(None);
    };
    if verify_password(password, stored) {
        Ok(Some(Principal {
            username: user.username,
            github_login: user.github_login,
            via: AuthVia::Session,
            grant: user.role.grant(),
            automation_context: None,
        }))
    } else {
        Ok(None)
    }
}

/// Build the loopback [`Principal`] — the primary user, marked [`AuthVia::Loopback`].
pub async fn loopback_principal(db: &Db) -> Result<Option<Principal>> {
    if github_organization_authorization_enabled(db).await? {
        return Ok(None);
    }
    let Some(username) = primary_user(db).await? else {
        return Ok(None);
    };
    Ok(get_user(db, &username).await?.map(|u| Principal {
        username: u.username,
        github_login: u.github_login,
        via: AuthVia::Loopback,
        grant: u.role.grant(),
        automation_context: None,
    }))
}

// ---------------------------------------------------------------------------
// Browser sessions (login cookies)
// ---------------------------------------------------------------------------

/// Open a browser session for `username`, returning the opaque cookie value.
pub async fn create_session(db: &Db, username: &str) -> Result<String> {
    let user = get_user(db, username)
        .await?
        .ok_or_else(|| anyhow!("no such user '{username}'"))?;
    let now = now_iso();
    if !user.is_authorized_at(&now) {
        return Err(anyhow!("authorization for user '{username}' has expired"));
    }
    let (plain, hash, _) = mint_token();
    let expires_at = iso_in_days(SESSION_TTL_DAYS)
        .ok_or_else(|| anyhow!("browser session expiry is outside the supported range"))?;
    sqlx::query("INSERT INTO auth_sessions (token_hash, username, expires_at) VALUES (?, ?, ?)")
        .bind(&hash)
        .bind(username)
        .bind(expires_at)
        .execute(db)
        .await?;
    Ok(plain)
}

/// Resolve a session cookie to its [`Principal`], or `None` if unknown, expired,
/// or its user has since been removed.
pub async fn lookup_session(db: &Db, cookie: &str) -> Result<Option<Principal>> {
    let hash = sha256_hex(cookie);
    let row = sqlx::query(
        "SELECT s.username AS username, u.github_login AS github_login, u.role AS role
         FROM auth_sessions s JOIN users u ON u.username = s.username
         WHERE s.token_hash = ? AND s.expires_at > ?
           AND (u.authorization_kind = 'manual' OR u.authorization_valid_until > ?)",
    )
    .bind(&hash)
    .bind(now_iso())
    .bind(now_iso())
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Principal {
        username: r.get("username"),
        github_login: r.get("github_login"),
        via: AuthVia::Session,
        grant: r.get::<UserRole, _>("role").grant(),
        automation_context: None,
    }))
}

/// Drop a session (logout). Best-effort — an unknown cookie is a no-op.
pub async fn delete_session(db: &Db, cookie: &str) -> Result<()> {
    let hash = sha256_hex(cookie);
    sqlx::query("DELETE FROM auth_sessions WHERE token_hash = ?")
        .bind(&hash)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// API tokens
// ---------------------------------------------------------------------------

/// 'pat' (a user-managed personal access token) or 'local' (the machine token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Pat,
    Local,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            TokenKind::Pat => "pat",
            TokenKind::Local => "local",
        }
    }
}

/// A token's non-secret metadata, for the token list.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

/// Mint a personal access token owned by `username`. Returns the one-time
/// plaintext plus the stored metadata.
pub async fn create_token(
    db: &Db,
    username: &str,
    name: &str,
    expires_in_days: Option<i64>,
) -> Result<(String, TokenInfo)> {
    create_token_kind(db, username, name, expires_in_days, TokenKind::Pat).await
}

async fn create_token_kind(
    db: &Db,
    username: &str,
    name: &str,
    expires_in_days: Option<i64>,
    kind: TokenKind,
) -> Result<(String, TokenInfo)> {
    let user = get_user(db, username)
        .await?
        .ok_or_else(|| anyhow!("no such user '{username}'"))?;
    if kind == TokenKind::Pat && !user.is_manually_authorized() {
        return Err(anyhow!(
            "organization-authorized users cannot create personal Loom API tokens"
        ));
    }
    let (plain, hash, prefix) = mint_token();
    let id = random_id();
    // A positive `expires_in_days` sets the expiry; anything else leaves the
    // token non-expiring.
    let expires_at = match expires_in_days {
        Some(d) if d > 0 => Some(
            iso_in_days(d).ok_or_else(|| anyhow!("token expiry is outside the supported range"))?,
        ),
        _ => None,
    };
    sqlx::query(
        "INSERT INTO api_tokens (id, username, name, token_hash, prefix, kind, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(name)
    .bind(&hash)
    .bind(&prefix)
    .bind(kind.as_str())
    .bind(&expires_at)
    .execute(db)
    .await
    .context("creating token")?;
    let info = get_token(db, &id)
        .await?
        .ok_or_else(|| anyhow!("token vanished after insert"))?;
    Ok((plain, info))
}

async fn get_token(db: &Db, id: &str) -> Result<Option<TokenInfo>> {
    let row = sqlx::query_as::<_, TokenInfo>(
        "SELECT id, name, prefix, created_at, last_used_at, expires_at FROM api_tokens WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Every user-managed token (the machine 'local' token is infrastructure and is
/// omitted), newest first.
pub async fn list_tokens(db: &Db, username: &str) -> Result<Vec<TokenInfo>> {
    let rows = sqlx::query_as::<_, TokenInfo>(
        "SELECT id, name, prefix, created_at, last_used_at, expires_at FROM api_tokens
         WHERE kind = 'pat' AND username = ? ORDER BY created_at DESC",
    )
    .bind(username)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Revoke a token by id. Refuses the machine 'local' token. Returns whether a
/// (revocable) row was removed.
pub async fn revoke_token(db: &Db, username: &str, id: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM api_tokens WHERE id = ? AND username = ? AND kind = 'pat'")
        .bind(id)
        .bind(username)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Resolve an `Authorization: Bearer` token to its [`Principal`]. Touches
/// `last_used_at` on a hit (best-effort). `None` for an unknown, expired, or
/// orphaned token.
pub async fn lookup_token(db: &Db, token: &str) -> Result<Option<Principal>> {
    if !token.starts_with(TOKEN_PREFIX) {
        return Ok(crate::automation::verify(db, token)
            .await?
            .map(|claims| Principal {
                username: claims.sub.clone(),
                github_login: None,
                via: AuthVia::Token,
                grant: Grant::Automation {
                    subject: claims.sub.clone(),
                    profiles: claims.profiles.clone(),
                },
                automation_context: claims.federation,
            }));
    }
    let hash = sha256_hex(token);
    let row = sqlx::query(
        "SELECT t.id AS id, t.username AS username, u.github_login AS github_login,
                u.role AS role, t.kind AS kind, t.grant_json AS grant_json,
                s.policy_restricted AS session_restricted
         FROM api_tokens t
         JOIN users u ON u.username = t.username
         LEFT JOIN sessions s ON s.id = t.bound_session_id
         WHERE t.token_hash = ? AND (t.expires_at IS NULL OR t.expires_at > ?)
           AND (u.authorization_kind = 'manual' OR u.authorization_valid_until > ?)
           AND (
             t.kind != 'session' OR s.status NOT IN ('done', 'error', 'archived')
           )",
    )
    .bind(&hash)
    .bind(now_iso())
    .bind(now_iso())
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: String = row.get("id");
    let kind: String = row.get("kind");
    if kind == TokenKind::Local.as_str() && github_organization_authorization_enabled(db).await? {
        return Ok(None);
    }
    let mut grant = if kind == TokenKind::Pat.as_str() || kind == TokenKind::Local.as_str() {
        row.get::<UserRole, _>("role").grant()
    } else {
        let grant_json: String = row.get("grant_json");
        match serde_json::from_str(&grant_json) {
            Ok(grant) => grant,
            Err(error) => {
                tracing::warn!(token_id = %id, %error, "rejecting token with invalid grant metadata");
                return Ok(None);
            }
        }
    };
    // Unrestricted session policy is forward-compatible by definition. Older
    // Loom versions serialized the then-current complete grant list, which
    // made a surviving token lose access whenever an upgrade registered a new
    // grant. Normalize those durable credentials as they authenticate; the
    // session row remains the source of truth and restricted credentials keep
    // their exact launch-time capabilities.
    if kind == "session" && row.get::<Option<bool>, _>("session_restricted") == Some(false) {
        if let Grant::Session { capabilities, .. } = &mut grant {
            *capabilities = None;
        }
    }
    let _ = sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
        .bind(now_iso())
        .bind(&id)
        .execute(db)
        .await;
    Ok(Some(Principal {
        username: row.get("username"),
        github_login: row.get("github_login"),
        via: AuthVia::Token,
        grant,
        automation_context: None,
    }))
}

/// Mint an opaque credential bound to exactly one session and branch. The
/// primary/admin user owns the row for lifecycle cleanup, while authorization
/// comes exclusively from the serialized session grant.
#[derive(Debug)]
pub struct StagedSessionToken {
    pub id: String,
    pub value: String,
}

pub fn session_capabilities_for_policy(
    restricted: bool,
    policy_mcp_access: &str,
) -> Result<Vec<String>> {
    let snapshot: weaver_api::McpPolicySnapshot =
        serde_json::from_str(policy_mcp_access).context("invalid session MCP policy snapshot")?;
    Ok(loom_agent::mcp::session_capabilities(
        restricted,
        snapshot
            .capability_sets
            .iter()
            .map(|capability| capability.name.as_str()),
    ))
}

fn session_token_capabilities_for_policy(
    restricted: bool,
    policy_mcp_access: &str,
) -> Result<Option<Vec<String>>> {
    if restricted {
        session_capabilities_for_policy(true, policy_mcp_access).map(Some)
    } else {
        Ok(None)
    }
}

async fn stored_session_capabilities(db: &Db, session_id: &str) -> Result<Option<Vec<String>>> {
    let row = sqlx::query("SELECT policy_restricted, policy_mcp_access FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(db)
        .await?;
    match row {
        Some(row) => session_token_capabilities_for_policy(
            row.get::<bool, _>("policy_restricted"),
            row.get::<String, _>("policy_mcp_access").as_str(),
        ),
        // Fresh provisioning mints before inserting the session so an agent's
        // first callback cannot race the token row. That path calls the
        // explicit-policy helper; retain unrestricted behavior as a safe
        // compatibility fallback for direct test fixtures.
        None => Ok(None),
    }
}

async fn mint_staged_session_token(
    db: &Db,
    owner: Option<&str>,
    session_id: &str,
    branch_id: &str,
    capabilities: Option<Vec<String>>,
) -> Result<StagedSessionToken> {
    let username = match owner {
        Some(username)
            if get_user(db, username)
                .await?
                .is_some_and(|user| user.is_authorized_at(&now_iso())) =>
        {
            username.to_string()
        }
        Some(username) => {
            return Err(anyhow!(
                "cannot mint a session credential for unauthorized user '{username}'"
            ));
        }
        _ => primary_user(db)
            .await?
            .ok_or_else(|| anyhow!("no primary user for session token"))?,
    };
    let (plain, hash, prefix) = mint_token();
    let id = random_id();
    let grant = serde_json::to_string(&Grant::Session {
        session_id: session_id.to_string(),
        branch_id: branch_id.to_string(),
        capabilities,
    })?;
    sqlx::query(
        "INSERT INTO api_tokens
         (id, username, name, token_hash, prefix, kind, grant_json, subject, bound_session_id)
         VALUES (?, ?, ?, ?, ?, 'session', ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(format!("session {session_id}"))
    .bind(hash)
    .bind(prefix)
    .bind(grant)
    .bind(session_id)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(StagedSessionToken { id, value: plain })
}

/// Mint a session credential without revoking any predecessor. Provider
/// handoff uses this staged form so a rejected preflight or teardown rollback
/// leaves the live source token fully usable.
pub async fn stage_session_token(
    db: &Db,
    owner: Option<&str>,
    session_id: &str,
    branch_id: &str,
) -> Result<StagedSessionToken> {
    let capabilities = stored_session_capabilities(db, session_id).await?;
    mint_staged_session_token(db, owner, session_id, branch_id, capabilities).await
}

pub async fn stage_session_token_with_policy(
    db: &Db,
    owner: Option<&str>,
    session_id: &str,
    branch_id: &str,
    restricted: bool,
    policy_mcp_access: &str,
) -> Result<StagedSessionToken> {
    let capabilities = session_token_capabilities_for_policy(restricted, policy_mcp_access)?;
    mint_staged_session_token(db, owner, session_id, branch_id, capabilities).await
}

pub async fn create_session_token(
    db: &Db,
    owner: Option<&str>,
    session_id: &str,
    branch_id: &str,
) -> Result<String> {
    Ok(stage_session_token(db, owner, session_id, branch_id)
        .await?
        .value)
}

pub async fn create_session_token_with_policy(
    db: &Db,
    owner: Option<&str>,
    session_id: &str,
    branch_id: &str,
    restricted: bool,
    policy_mcp_access: &str,
) -> Result<String> {
    Ok(stage_session_token_with_policy(
        db,
        owner,
        session_id,
        branch_id,
        restricted,
        policy_mcp_access,
    )
    .await?
    .value)
}

/// Roll back one uncommitted replacement credential without touching the live
/// source session's other tokens.
pub async fn revoke_staged_session_token(db: &Db, token_id: &str) -> Result<bool> {
    Ok(
        sqlx::query("DELETE FROM api_tokens WHERE id = ? AND kind = 'session'")
            .bind(token_id)
            .execute(db)
            .await?
            .rows_affected()
            == 1,
    )
}

/// Commit a replacement credential by retaining exactly `token_id` for the
/// session and revoking only superseded session tokens.
pub async fn commit_staged_session_token(db: &Db, session_id: &str, token_id: &str) -> Result<u64> {
    Ok(sqlx::query(
        "DELETE FROM api_tokens
         WHERE kind = 'session' AND bound_session_id = ? AND id <> ?",
    )
    .bind(session_id)
    .bind(token_id)
    .execute(db)
    .await?
    .rows_affected())
}

pub async fn revoke_session_tokens(db: &Db, session_id: &str) -> Result<u64> {
    Ok(
        sqlx::query("DELETE FROM api_tokens WHERE kind = 'session' AND bound_session_id = ?")
            .bind(session_id)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

// ---------------------------------------------------------------------------
// The machine-local token
// ---------------------------------------------------------------------------

pub use crate::paths::local_token_path;

/// Ensure the machine-local bearer token exists and return its plaintext.
///
/// loom injects this into the environments of its own same-host subprocesses
/// (the agent's terminal, watch scripts) and the `loom` CLI reads it, so local
/// automation authenticates even when `auth.trust_loopback` is off. The
/// plaintext is persisted (0600) under `$WEAVER_HOME` and reused across
/// restarts; if the database is reset but the file survives, the same plaintext
/// is re-registered so existing subprocesses keep working.
pub async fn ensure_local_token(db: &Db) -> Result<String> {
    let path = local_token_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let plain = existing.trim().to_string();
        if !plain.is_empty() {
            let hash = sha256_hex(&plain);
            let known = sqlx::query("SELECT 1 AS ok FROM api_tokens WHERE token_hash = ?")
                .bind(&hash)
                .fetch_optional(db)
                .await?
                .is_some();
            if !known {
                register_local_token(db, &plain).await?;
            }
            return Ok(plain);
        }
    }
    let (plain, _, _) = mint_token();
    write_private(&path, &plain)?;
    register_local_token(db, &plain).await?;
    Ok(plain)
}

/// Register a known plaintext as the machine 'local' token row, owned by the
/// primary user. Idempotent on the hash.
async fn register_local_token(db: &Db, plain: &str) -> Result<()> {
    let owner = primary_user(db)
        .await?
        .ok_or_else(|| anyhow!("no users seeded — cannot register the local token"))?;
    let hash = sha256_hex(plain);
    let prefix: String = plain.chars().take(PREFIX_KEEP).collect();
    sqlx::query(
        "INSERT INTO api_tokens (id, username, name, token_hash, prefix, kind)
         VALUES (?, ?, ?, ?, ?, 'local')
         ON CONFLICT DO NOTHING",
    )
    .bind(random_id())
    .bind(&owner)
    .bind(LOCAL_TOKEN_NAME)
    .bind(&hash)
    .bind(&prefix)
    .execute(db)
    .await
    .context("registering the local token")?;
    Ok(())
}

/// Write `contents` to `path` with owner-only (0600) permissions.
fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GitHub OAuth
// ---------------------------------------------------------------------------

/// Settings keys (also overridable by the env vars in [`github_oauth`]).
pub const GH_CLIENT_ID_KEY: &str = "auth.github.client_id";
pub const GH_CLIENT_SECRET_KEY: &str = "auth.github.client_secret";
pub const GH_ORGANIZATIONS_KEY: &str = weaver_core::config::GITHUB_ORGANIZATIONS_KEY;
const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const GITHUB_MEMBERSHIP_TIMEOUT_SECONDS: u64 = 10;

/// A configured GitHub OAuth app.
#[derive(Debug, Clone)]
pub struct GithubOAuth {
    pub client_id: String,
    pub client_secret: String,
}

async fn env_or_setting(db: &Db, env: &str, key: &str) -> String {
    if let Ok(v) = std::env::var(env) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    crate::config::get(db, key).await.unwrap_or_default()
}

/// The configured OAuth client id (env-or-settings), or empty when unset —
/// the public half shown in the settings UI. Resolved the same way as
/// [`github_oauth`] so the two stay in sync.
pub async fn oauth_client_id(db: &Db) -> String {
    env_or_setting(db, "LOOM_GITHUB_CLIENT_ID", GH_CLIENT_ID_KEY).await
}

/// The GitHub OAuth app config, or `None` when sign-in-with-GitHub is not set
/// up. Reads `LOOM_GITHUB_CLIENT_ID`/`_SECRET` first, then the settings table.
pub async fn github_oauth(db: &Db) -> Option<GithubOAuth> {
    let client_id = env_or_setting(db, "LOOM_GITHUB_CLIENT_ID", GH_CLIENT_ID_KEY).await;
    let client_secret = env_or_setting(db, "LOOM_GITHUB_CLIENT_SECRET", GH_CLIENT_SECRET_KEY).await;
    if client_id.is_empty() || client_secret.is_empty() {
        return None;
    }
    Some(GithubOAuth {
        client_id,
        client_secret,
    })
}

/// The URL to send the browser to, to begin the OAuth dance. `state` is the
/// CSRF nonce echoed back to the callback; `redirect_uri` is loom's callback.
pub fn authorize_url(cfg: &GithubOAuth, state: &str, redirect_uri: &str) -> String {
    let q = |s: &str| {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=read:user&state={}",
        q(&cfg.client_id),
        q(redirect_uri),
        q(state),
    )
}

/// Mask GitHub token-shaped secrets in a response body before it goes into a log
/// line or error. GitHub's token endpoint can echo the access token in its body
/// (JSON `"access_token":"…"`, or form-encoded `access_token=…` if it ignores our
/// JSON `Accept`); every GitHub token carries a recognisable prefix, so blanking
/// the run after one keeps the body diagnostic without exposing a usable
/// credential.
fn redact_secrets(body: &str) -> String {
    const PREFIXES: [&str; 6] = ["gho_", "ghu_", "ghs_", "ghr_", "ghp_", "github_pat_"];
    let mut out = body.to_string();
    for prefix in PREFIXES {
        let mut from = 0;
        while let Some(rel) = out[from..].find(prefix) {
            let start = from + rel;
            let secret_start = start + prefix.len();
            let secret_len = out[secret_start..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(out.len() - secret_start);
            out.replace_range(start..secret_start + secret_len, "<redacted-token>");
            from = start + "<redacted-token>".len();
        }
    }
    out
}

/// A response body trimmed for a log/error line: secrets masked, length capped.
fn redacted_snippet(body: &str) -> String {
    redact_secrets(body).chars().take(500).collect()
}

/// Exchange an OAuth `code` for a GitHub access token.
pub async fn exchange_code(cfg: &GithubOAuth, code: &str, redirect_uri: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        scope: Option<String>,
        error: Option<String>,
        error_description: Option<String>,
    }
    let http = reqwest::Client::new()
        .post("https://github.com/login/oauth/access_token")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "loom")
        .json(&serde_json::json!({
            "client_id": cfg.client_id,
            "client_secret": cfg.client_secret,
            "code": code,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await
        .context("requesting GitHub access token")?;
    // GitHub returns HTTP 200 even for OAuth errors (the failure is in the body),
    // so read the raw body and report the status + payload on any problem rather
    // than discarding them — these are the only clues when sign-in breaks.
    let status = http.status();
    let body = http.text().await.context("reading GitHub token response")?;
    let resp: TokenResp = serde_json::from_str(&body).with_context(|| {
        format!(
            "decoding GitHub token response (HTTP {status}): {}",
            redacted_snippet(&body)
        )
    })?;
    match resp.access_token {
        Some(token) if !token.is_empty() => {
            tracing::debug!(
                token_prefix = %token.chars().take(4).collect::<String>(),
                scope = resp.scope.as_deref().unwrap_or(""),
                "exchanged GitHub OAuth code for an access token"
            );
            Ok(token)
        }
        _ => {
            let detail = resp
                .error_description
                .or(resp.error)
                .unwrap_or_else(|| redacted_snippet(&body));
            tracing::warn!(%status, redirect_uri, "GitHub token exchange returned no access token: {detail}");
            Err(anyhow!(
                "GitHub did not return an access token (HTTP {status}): {detail}"
            ))
        }
    }
}

/// The authenticated user's GitHub profile, as much as sign-in needs: `login`
/// for the allowlist check, and `id`/`name` for commit attribution. `name` is
/// the free-text profile name and may be absent.
pub struct GithubUser {
    pub login: String,
    pub id: i64,
    pub name: Option<String>,
}

/// Whether external GitHub organization authorization puts this Loom instance
/// in shared-deployment mode.
pub async fn github_organization_authorization_enabled(db: &Db) -> Result<bool> {
    if crate::config::try_get(
        db,
        weaver_core::config::GITHUB_ORGANIZATION_SHARED_MODE_LATCH_KEY,
    )
    .await?
    .is_some()
    {
        return Ok(true);
    }
    let configured = crate::config::try_get(db, GH_ORGANIZATIONS_KEY)
        .await?
        .unwrap_or_default();
    let organizations = crate::config::parse_github_organizations(&configured)
        .map_err(|error| anyhow!("invalid {GH_ORGANIZATIONS_KEY}: {error}"))?;
    if !organizations.is_empty() {
        crate::config::latch_github_organization_shared_mode(db).await?;
        return Ok(true);
    }
    let derived_authority = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
             SELECT 1 FROM users WHERE authorization_kind = 'github_organization'
         )",
    )
    .fetch_one(db)
    .await?;
    if derived_authority {
        crate::config::latch_github_organization_shared_mode(db).await?;
    }
    Ok(derived_authority)
}

/// Every currently configured organization. Authorization is the union of
/// these memberships; callers must check all entries because a user can move
/// between configured organizations without signing in again.
pub async fn configured_github_organizations(
    db: &Db,
) -> Result<Vec<crate::config::GithubOrganization>> {
    let configured = crate::config::try_get(db, GH_ORGANIZATIONS_KEY)
        .await?
        .unwrap_or_default();
    crate::config::parse_github_organizations(&configured)
        .map_err(|error| anyhow!("invalid {GH_ORGANIZATIONS_KEY}: {error}"))
}

async fn user_by_github_id(db: &Db, github_user_id: i64) -> Result<Option<User>> {
    sqlx::query_as::<_, User>(
        "SELECT username, github_login, github_user_id, password_hash, role, authorization_kind,
                authorization_github_org_id, authorization_github_org_login,
                authorization_valid_until, created_at
         FROM users WHERE github_user_id = ?",
    )
    .bind(github_user_id)
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

/// Match an authenticated GitHub profile only by its durable numeric id.
/// A login-only manual row is never converted into authority because GitHub
/// permits renamed logins to be reclaimed by a different account.
async fn bind_github_identity(db: &Db, github: &GithubUser) -> Result<Option<User>> {
    if user_by_github_id(db, github.id).await?.is_some() {
        sqlx::query("UPDATE users SET github_login = ?, display_name = ? WHERE github_user_id = ?")
            .bind(&github.login)
            .bind(&github.name)
            .bind(github.id)
            .execute(db)
            .await?;
        return user_by_github_id(db, github.id).await;
    }

    let login_binding = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT github_user_id FROM users
         WHERE github_login IS NOT NULL AND lower(github_login) = lower(?)",
    )
    .bind(&github.login)
    .fetch_optional(db)
    .await?;
    if let Some(binding) = login_binding {
        if let Some(bound_id) = binding {
            return Err(anyhow!(
                "GitHub login '{}' belongs to id {}, but Loom bound it to id {}",
                github.login,
                github.id,
                bound_id
            ));
        }
        return Err(anyhow!(
            "GitHub login '{}' has no trusted numeric id; an administrator must bind the identity before it can sign in",
            github.login
        ));
    }
    Ok(None)
}

fn github_organization_valid_until() -> Result<String> {
    let delta = chrono::TimeDelta::try_hours(GITHUB_ORGANIZATION_LEASE_HOURS)
        .ok_or_else(|| anyhow!("GitHub organization lease is outside the supported range"))?;
    chrono::Utc::now()
        .checked_add_signed(delta)
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .ok_or_else(|| anyhow!("GitHub organization lease expiry is outside the supported range"))
}

#[derive(Debug)]
enum GithubMembership {
    Active,
    Inactive,
    Indeterminate(String),
}

/// Resolve a GitHub identity to a currently authorized Loom user. Manual
/// approvals win without an organization request. Organization-derived users
/// receive a one-hour lease that every Loom credential lookup enforces.
pub async fn approved_github_user(
    db: &Db,
    access_token: &str,
    github: &GithubUser,
) -> Result<Option<User>> {
    approved_github_user_at(db, access_token, github, GITHUB_API_BASE).await
}

async fn approved_github_user_at(
    db: &Db,
    access_token: &str,
    github: &GithubUser,
    api_base: &str,
) -> Result<Option<User>> {
    let existing = bind_github_identity(db, github).await?;
    if existing.as_ref().is_some_and(User::is_manually_authorized) {
        return Ok(existing);
    }
    let previous_authorization = existing
        .as_ref()
        .map(|user| github_organization_authorization_from_user(user, github.id))
        .transpose()?
        .flatten();

    let organizations = configured_github_organizations(db).await?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            GITHUB_MEMBERSHIP_TIMEOUT_SECONDS,
        ))
        .build()?;
    let mut indeterminate = Vec::new();
    let mut active = None;
    for organization in organizations {
        match github_organization_membership_at(&http, api_base, access_token, &organization).await
        {
            GithubMembership::Active => {
                active = Some(organization);
                break;
            }
            GithubMembership::Inactive => {}
            GithubMembership::Indeterminate(reason) => indeterminate.push(reason),
        }
    }

    if let Some(organization) = active {
        if let Some(previous_authorization) = previous_authorization.as_ref() {
            let renewed = renew_github_organization_authorization(
                db,
                previous_authorization,
                &github.login,
                &organization,
            )
            .await?;
            if !renewed {
                let current = user_by_github_id(db, github.id).await?;
                if current
                    .as_ref()
                    .is_some_and(|user| user.is_authorized_at(&now_iso()))
                {
                    return Ok(current);
                }
                return Err(anyhow!(
                    "GitHub identity '{}' changed while its organization lease was refreshed",
                    github.login
                ));
            }
        } else {
            let valid_until = github_organization_valid_until()?;
            sqlx::query(
                "INSERT INTO users
                 (username, github_login, github_user_id, display_name, role,
                  authorization_kind, authorization_github_org_id,
                  authorization_github_org_login, authorization_valid_until)
                 VALUES (?, ?, ?, ?, 'user', 'github_organization', ?, ?, ?)",
            )
            .bind(&github.login)
            .bind(&github.login)
            .bind(github.id)
            .bind(&github.name)
            .bind(organization.id)
            .bind(&organization.login)
            .bind(&valid_until)
            .execute(db)
            .await
            .with_context(|| {
                format!(
                    "creating GitHub organization-authorized user '{}'",
                    github.login
                )
            })?;
        }
        let user = user_by_github_id(db, github.id).await?.ok_or_else(|| {
            anyhow!(
                "GitHub user '{}' vanished after authorization",
                github.login
            )
        })?;
        tracing::info!(
            login = %github.login,
            github_user_id = github.id,
            organization = %organization.login,
            github_organization_id = organization.id,
            valid_until = ?user.authorization_valid_until,
            "renewed GitHub organization authorization"
        );
        return Ok(Some(user));
    }

    if let Some(previous_authorization) = previous_authorization.as_ref() {
        let expired = expire_github_organization_authorization(db, previous_authorization).await?;
        if !expired {
            let current = user_by_github_id(db, github.id).await?;
            if current
                .as_ref()
                .is_some_and(|user| user.is_authorized_at(&now_iso()))
            {
                return Ok(current);
            }
        }
    }
    if !indeterminate.is_empty() {
        return Err(anyhow!(
            "GitHub organization membership could not be verified: {}",
            indeterminate.join("; ")
        ));
    }
    Ok(None)
}

async fn github_organization_membership_at(
    http: &reqwest::Client,
    api_base: &str,
    access_token: &str,
    organization: &crate::config::GithubOrganization,
) -> GithubMembership {
    #[derive(Deserialize)]
    struct Membership {
        state: String,
        organization: MembershipOrganization,
    }
    #[derive(Deserialize)]
    struct MembershipOrganization {
        id: i64,
        login: String,
    }

    let response = match http
        .get(format!(
            "{}/user/memberships/orgs/{}",
            api_base.trim_end_matches('/'),
            organization.login
        ))
        .header(reqwest::header::USER_AGENT, "loom")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .bearer_auth(access_token)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return GithubMembership::Indeterminate(format!(
                "{} request failed: {error}",
                organization.login
            ));
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return GithubMembership::Inactive;
    }
    if !status.is_success() {
        let detail = match response.text().await {
            Ok(body) => redacted_snippet(&body),
            Err(error) => format!("<failed to read response body: {error}>"),
        };
        return GithubMembership::Indeterminate(format!(
            "{} returned HTTP {status}: {detail}",
            organization.login
        ));
    }
    let membership: Membership = match response.json().await {
        Ok(membership) => membership,
        Err(error) => {
            return GithubMembership::Indeterminate(format!(
                "{} returned malformed membership data: {error}",
                organization.login
            ));
        }
    };
    if membership.organization.id != organization.id {
        return GithubMembership::Indeterminate(format!(
            "{} resolved to GitHub organization id {}, expected {}",
            organization.login, membership.organization.id, organization.id
        ));
    }
    if !membership
        .organization
        .login
        .eq_ignore_ascii_case(&organization.login)
    {
        return GithubMembership::Indeterminate(format!(
            "GitHub returned organization login '{}' for configured login '{}'",
            membership.organization.login, organization.login
        ));
    }
    if membership.state.eq_ignore_ascii_case("active") {
        GithubMembership::Active
    } else {
        GithubMembership::Inactive
    }
}

/// Fetch the authenticated user's GitHub profile for `access_token`.
pub async fn fetch_github_user(access_token: &str) -> Result<GithubUser> {
    #[derive(serde::Deserialize)]
    struct GhUser {
        login: String,
        id: i64,
        name: Option<String>,
    }
    let resp = reqwest::Client::new()
        .get("https://api.github.com/user")
        .header(reqwest::header::USER_AGENT, "loom")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .bearer_auth(access_token)
        .send()
        .await
        .context("fetching GitHub user")?;
    // Report GitHub's status + body instead of a bare "request failed" — a 401
    // here means the token was rejected, which is otherwise invisible.
    let status = resp.status();
    if !status.is_success() {
        let detail = match resp.text().await {
            Ok(body) => redacted_snippet(&body),
            Err(e) => format!("<failed to read response body: {e}>"),
        };
        tracing::warn!(%status, "GitHub /user request failed: {detail}");
        return Err(anyhow!(
            "GitHub user request failed (HTTP {status}): {detail}"
        ));
    }
    let user: GhUser = resp.json().await.context("decoding GitHub user")?;
    Ok(GithubUser {
        login: user.login,
        id: user.id,
        name: user.name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use axum::extract::Path as AxumPath;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};

    #[test]
    fn redact_secrets_masks_github_tokens_anywhere() {
        // JSON and form bodies that echo a token are both scrubbed...
        let json = r#"{"access_token":"gho_AbC123def456","scope":"read:user"}"#;
        let form = "access_token=ghu_xyz789&token_type=bearer";
        for body in [json, form] {
            let red = redact_secrets(body);
            assert!(
                !red.contains("gho_AbC123def456"),
                "json token leaked: {red}"
            );
            assert!(!red.contains("ghu_xyz789"), "form token leaked: {red}");
            assert!(red.contains("<redacted-token>"));
        }
        // ...while the surrounding, non-secret structure is preserved.
        assert!(redact_secrets(json).contains("\"scope\":\"read:user\""));
        // A benign body with no token is returned unchanged (and terminates).
        let benign = r#"{"message":"Bad credentials"}"#;
        assert_eq!(redact_secrets(benign), benign);
    }

    #[test]
    fn minted_tokens_are_prefixed_unique_and_hash_consistently() {
        let (a, ah, ap) = mint_token();
        let (b, _, _) = mint_token();
        assert!(a.starts_with("loom_"));
        assert_ne!(a, b, "two mints must differ");
        assert_eq!(sha256_hex(&a), ah, "stored hash must match the plaintext");
        assert!(a.starts_with(&ap), "prefix is a leading slice of the token");
        assert_eq!(ap.len(), PREFIX_KEEP);
    }

    #[test]
    fn password_hash_roundtrips_and_rejects_wrong_password() {
        let hash = hash_password("hunter2").unwrap();
        assert!(verify_password("hunter2", &hash));
        assert!(!verify_password("hunter3", &hash));
        // A garbage stored hash fails closed.
        assert!(!verify_password("hunter2", "not-a-hash"));
    }

    /// `db::connect_in_memory`, with `LOOM_OWNER_GITHUB` set so `seed_owner`
    /// plants an owner. Every test that relies on a pre-seeded owner must set
    /// one explicitly. The caller must be `#[serial]`: the env var is global.
    async fn connect_in_memory_with_owner(owner: &str) -> Db {
        std::env::set_var("LOOM_OWNER_GITHUB", owner);
        std::env::set_var("LOOM_OWNER_GITHUB_ID", "4242");
        let db = db::connect_in_memory().await.unwrap();
        std::env::remove_var("LOOM_OWNER_GITHUB");
        std::env::remove_var("LOOM_OWNER_GITHUB_ID");
        db
    }

    async fn github_organization_api() -> String {
        async fn membership(
            AxumPath(organization): AxumPath<String>,
            headers: HeaderMap,
        ) -> Response {
            assert_eq!(
                headers
                    .get(reqwest::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer github-user-token")
            );
            match organization.as_str() {
                "Open-Athena" => Json(serde_json::json!({
                    "state": "active",
                    "organization": {"id": 188075292, "login": "Open-Athena"}
                }))
                .into_response(),
                "Pending" => Json(serde_json::json!({
                    "state": "pending",
                    "organization": {"id": 200, "login": "Pending"}
                }))
                .into_response(),
                "WrongId" => Json(serde_json::json!({
                    "state": "active",
                    "organization": {"id": 999, "login": "WrongId"}
                }))
                .into_response(),
                "Malformed" => (StatusCode::OK, "not-json").into_response(),
                "Forbidden" => (StatusCode::FORBIDDEN, "App needs Members: read").into_response(),
                "RateLimited" => StatusCode::TOO_MANY_REQUESTS.into_response(),
                "Error" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/user/memberships/orgs/{organization}", get(membership)),
            )
            .await
            .unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn unset_owner_seeds_no_user_at_all() {
        std::env::remove_var("LOOM_OWNER_GITHUB");
        std::env::remove_var("LOOM_OWNER_GITHUB_ID");
        let db = db::connect_in_memory().await.unwrap();
        assert_eq!(primary_user(&db).await.unwrap(), None);
        assert!(list_users(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn seeded_owner_is_the_primary_user() {
        let db = connect_in_memory_with_owner("rjpower").await;
        assert_eq!(primary_user(&db).await.unwrap().as_deref(), Some("rjpower"));
        let u = user_by_github(&db, "RJPower").await.unwrap();
        let u = u.unwrap();
        assert_eq!(u.username, "rjpower");
        assert_eq!(u.github_user_id, Some(4242));
        assert_eq!(u.role, UserRole::Admin);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn github_organization_members_receive_a_renewable_lease() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let api = github_organization_api().await;
        crate::config::apply(
            &db,
            &[(
                GH_ORGANIZATIONS_KEY.to_string(),
                Some("Missing:1, Open-Athena:188075292".to_string()),
            )],
        )
        .await
        .unwrap();
        let (local_token, _) =
            create_token_kind(&db, "rjpower", LOCAL_TOKEN_NAME, None, TokenKind::Local)
                .await
                .unwrap();
        assert!(lookup_token(&db, &local_token).await.unwrap().is_none());
        assert!(loopback_principal(&db).await.unwrap().is_none());

        let github = GithubUser {
            login: "new-member".to_string(),
            id: 42,
            name: Some("New Member".to_string()),
        };
        let user = approved_github_user_at(&db, "github-user-token", &github, &api)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.username, "new-member");
        assert_eq!(user.role, UserRole::User);
        assert_eq!(
            user.authorization_kind,
            UserAuthorizationKind::GithubOrganization
        );
        assert_eq!(user.authorization_github_org_id, Some(188075292));
        assert!(user.is_authorized_at(&now_iso()));

        crate::config::apply(
            &db,
            &[(GH_ORGANIZATIONS_KEY.to_string(), Some(String::new()))],
        )
        .await
        .unwrap();
        assert!(github_organization_authorization_enabled(&db)
            .await
            .unwrap());
        assert!(lookup_token(&db, &local_token).await.unwrap().is_none());
        assert!(loopback_principal(&db).await.unwrap().is_none());

        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/org", "main")
            .await
            .unwrap();
        crate::session::insert(
            &db,
            &crate::session::NewSession {
                id: "session-id".to_string(),
                branch_id: branch.id.clone(),
                work_dir: "/worktree".to_string(),
                term_session: "term-session".to_string(),
                agent_kind: "shell".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: Some(user.username.clone()),
                protocol: "terminal".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        let cookie = create_session(&db, &user.username).await.unwrap();
        let session_token =
            mint_staged_session_token(&db, Some(&user.username), "session-id", &branch.id, None)
                .await
                .unwrap();
        assert!(lookup_session(&db, &cookie).await.unwrap().is_some());
        assert!(lookup_token(&db, &session_token.value)
            .await
            .unwrap()
            .is_some());
        assert!(create_token(&db, &user.username, "bypass", None)
            .await
            .is_err());
        assert!(set_password(&db, &user.username, Some("password"))
            .await
            .is_err());

        sqlx::query(
            "UPDATE users SET authorization_valid_until = '2000-01-01T00:00:00.000Z'
             WHERE github_user_id = 42",
        )
        .execute(&db)
        .await
        .unwrap();
        assert!(lookup_session(&db, &cookie).await.unwrap().is_none());
        assert!(lookup_token(&db, &session_token.value)
            .await
            .unwrap()
            .is_none());

        let due = github_organization_authorizations_due(&db).await.unwrap();
        assert_eq!(due.len(), 1);
        let stale_authorization = due[0].clone();
        assert!(renew_github_organization_authorization(
            &db,
            &stale_authorization,
            "renamed-member",
            &crate::config::GithubOrganization {
                login: "Open-Athena".to_string(),
                id: 188075292,
            }
        )
        .await
        .unwrap());
        assert!(lookup_session(&db, &cookie).await.unwrap().is_some());
        assert!(
            !expire_github_organization_authorization(&db, &stale_authorization)
                .await
                .unwrap()
        );
        assert_eq!(
            user_by_github_id(&db, github.id)
                .await
                .unwrap()
                .unwrap()
                .github_login
                .as_deref(),
            Some("renamed-member")
        );
        sqlx::query(
            "UPDATE users SET authorization_valid_until = '2000-01-01T00:00:00.000Z'
             WHERE github_user_id = 42",
        )
        .execute(&db)
        .await
        .unwrap();

        // A later active organization still wins over an earlier indeterminate
        // result; a no-active outcome then leaves the lease expired.
        crate::config::apply(
            &db,
            &[(
                GH_ORGANIZATIONS_KEY.to_string(),
                Some("Forbidden:300, Open-Athena:188075292".to_string()),
            )],
        )
        .await
        .unwrap();
        assert!(
            approved_github_user_at(&db, "github-user-token", &github, &api)
                .await
                .unwrap()
                .is_some()
        );

        crate::config::apply(
            &db,
            &[(
                GH_ORGANIZATIONS_KEY.to_string(),
                Some("WrongId:400, Forbidden:300".to_string()),
            )],
        )
        .await
        .unwrap();
        assert!(
            approved_github_user_at(&db, "github-user-token", &github, &api)
                .await
                .unwrap_err()
                .to_string()
                .contains("expected 400")
        );
        assert_eq!(
            expired_github_organization_username(&db, github.id)
                .await
                .unwrap()
                .as_deref(),
            Some("new-member")
        );

        let manual = approve_user_manually(&db, &user.username).await.unwrap();
        assert!(manual.is_manually_authorized());
        set_user_role(&db, &user.username, UserRole::Admin)
            .await
            .unwrap();
        assert!(set_password(&db, &user.username, Some("password"))
            .await
            .is_ok());
        crate::config::apply(
            &db,
            &[(GH_ORGANIZATIONS_KEY.to_string(), Some(String::new()))],
        )
        .await
        .unwrap();
        assert!(github_organization_authorization_enabled(&db)
            .await
            .unwrap());
        assert!(lookup_token(&db, &local_token).await.unwrap().is_none());
        assert!(loopback_principal(&db).await.unwrap().is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn github_identity_uses_numeric_id_across_login_changes() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let original = GithubUser {
            login: "rjpower".to_string(),
            id: 4242,
            name: None,
        };
        let user = approved_github_user_at(&db, "unused", &original, "http://127.0.0.1:1")
            .await
            .unwrap()
            .unwrap();
        assert!(user.is_manually_authorized());

        let renamed = GithubUser {
            login: "new-login".to_string(),
            ..original
        };
        let user = approved_github_user_at(&db, "unused", &renamed, "http://127.0.0.1:1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.username, "rjpower");
        assert_eq!(user.github_login.as_deref(), Some("new-login"));

        let reclaimed = GithubUser {
            login: "rjpower".to_string(),
            id: 9999,
            name: None,
        };
        assert!(
            approved_github_user_at(&db, "unused", &reclaimed, "http://127.0.0.1:1")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn login_only_manual_approval_never_binds_during_oauth() {
        let db = db::connect_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO users (username, github_login, role) VALUES ('old-login', 'old-login', 'admin')",
        )
        .execute(&db)
        .await
        .unwrap();
        let reclaimed = GithubUser {
            login: "old-login".to_string(),
            id: 9999,
            name: None,
        };
        assert!(
            approved_github_user_at(&db, "unused", &reclaimed, "http://127.0.0.1:1")
                .await
                .unwrap_err()
                .to_string()
                .contains("trusted numeric id")
        );
        let id = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT github_user_id FROM users WHERE username = 'old-login'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(id, None);
        set_manual_github_identity(&db, "old-login", "old-login", 42)
            .await
            .unwrap();
        let intended = GithubUser {
            login: "old-login".to_string(),
            id: 42,
            name: None,
        };
        assert!(
            approved_github_user_at(&db, "unused", &intended, "http://127.0.0.1:1")
                .await
                .unwrap()
                .unwrap()
                .is_manually_authorized()
        );
    }

    #[tokio::test]
    async fn unreadable_organization_config_never_enables_local_trust() {
        let db = db::connect_in_memory().await.unwrap();
        sqlx::query("DROP TABLE settings")
            .execute(&db)
            .await
            .unwrap();
        assert!(github_organization_authorization_enabled(&db)
            .await
            .is_err());
        assert!(loopback_principal(&db).await.is_err());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn commit_identity_derives_noreply_email() {
        let db = connect_in_memory_with_owner("rjpower").await;

        // Before a display name is captured, the trusted numeric id still
        // produces the stable account-linked noreply email.
        let id = commit_identity(&db, "rjpower").await.unwrap().unwrap();
        assert_eq!(id.name, "rjpower");
        assert_eq!(id.email, "4242+rjpower@users.noreply.github.com");

        // After sign-in records the numeric id + display name: the stable
        // account-linked noreply email, and the display name as the git author.
        bind_github_identity(
            &db,
            &GithubUser {
                login: "RJPower".to_string(),
                id: 4242,
                name: Some("Russell Power".to_string()),
            },
        )
        .await
        .unwrap();
        let id = commit_identity(&db, "rjpower").await.unwrap().unwrap();
        assert_eq!(id.name, "Russell Power");
        assert_eq!(id.email, "4242+RJPower@users.noreply.github.com");

        // A password-only operator has no GitHub identity to attribute to.
        add_user(&db, "localonly", None, None, Some("pw"), UserRole::User)
            .await
            .unwrap();
        assert!(commit_identity(&db, "localonly").await.unwrap().is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn token_lifecycle_create_lookup_revoke() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let (plain, info) = create_token(&db, "rjpower", "ci", None).await.unwrap();

        let p = lookup_token(&db, &plain)
            .await
            .unwrap()
            .expect("valid token");
        assert_eq!(p.username, "rjpower");
        assert_eq!(p.via, AuthVia::Token);

        assert_eq!(list_tokens(&db, "rjpower").await.unwrap().len(), 1);
        assert!(revoke_token(&db, "rjpower", &info.id).await.unwrap());
        assert!(lookup_token(&db, &plain).await.unwrap().is_none());
        assert!(list_tokens(&db, "rjpower").await.unwrap().is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn human_credentials_follow_role_changes_and_tokens_are_private() {
        let db = connect_in_memory_with_owner("rjpower").await;
        add_user(
            &db,
            "alice",
            Some("alice-gh"),
            Some(101),
            Some("alice-password"),
            UserRole::User,
        )
        .await
        .unwrap();
        let cookie = create_session(&db, "alice").await.unwrap();
        let (alice_token, alice_info) =
            create_token(&db, "alice", "alice-cli", None).await.unwrap();
        let (_, owner_info) = create_token(&db, "rjpower", "owner-cli", None)
            .await
            .unwrap();

        assert_eq!(
            lookup_session(&db, &cookie).await.unwrap().unwrap().grant,
            Grant::User
        );
        assert_eq!(
            lookup_token(&db, &alice_token)
                .await
                .unwrap()
                .unwrap()
                .grant,
            Grant::User
        );
        assert_eq!(list_tokens(&db, "alice").await.unwrap().len(), 1);
        assert!(!revoke_token(&db, "alice", &owner_info.id).await.unwrap());

        set_user_role(&db, "alice", UserRole::Admin).await.unwrap();
        assert_eq!(
            lookup_session(&db, &cookie).await.unwrap().unwrap().grant,
            Grant::Admin
        );
        assert_eq!(
            lookup_token(&db, &alice_token)
                .await
                .unwrap()
                .unwrap()
                .grant,
            Grant::Admin
        );

        set_user_role(&db, "rjpower", UserRole::User).await.unwrap();
        assert!(set_user_role(&db, "alice", UserRole::User).await.is_err());
        assert!(revoke_token(&db, "alice", &alice_info.id).await.unwrap());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn personal_preferences_apply_and_reset_per_user() {
        let db = connect_in_memory_with_owner("rjpower").await;
        add_user(&db, "alice", None, None, None, UserRole::User)
            .await
            .unwrap();
        apply_user_preferences(
            &db,
            "rjpower",
            &[("terminal.theme".to_string(), Some("light".to_string()))],
        )
        .await
        .unwrap();
        assert_eq!(
            user_preferences(&db, "rjpower").await.unwrap(),
            HashMap::from([("terminal.theme".to_string(), "light".to_string())])
        );
        assert!(user_preferences(&db, "alice").await.unwrap().is_empty());

        apply_user_preferences(&db, "rjpower", &[("terminal.theme".to_string(), None)])
            .await
            .unwrap();
        assert!(user_preferences(&db, "rjpower").await.unwrap().is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn session_tokens_are_scoped_and_revoked_by_session() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/scoped", "main")
            .await
            .unwrap();
        crate::session::insert(
            &db,
            &crate::session::NewSession {
                id: "s1".to_string(),
                branch_id: branch.id.clone(),
                work_dir: "/w".to_string(),
                term_session: "weaver-s1".to_string(),
                agent_kind: "shell".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: Some("rjpower".to_string()),
                protocol: "terminal".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        let plain = create_session_token(&db, Some("rjpower"), "s1", &branch.id)
            .await
            .unwrap();
        let principal = lookup_token(&db, &plain).await.unwrap().unwrap();
        let Grant::Session {
            session_id,
            branch_id,
            capabilities,
        } = principal.grant
        else {
            panic!("expected a session grant");
        };
        assert_eq!(session_id, "s1");
        assert_eq!(branch_id, branch.id);
        assert_eq!(capabilities, None);

        // Credentials minted by older Loom versions froze an unrestricted
        // session to the complete capability list that existed at the time.
        // Authentication upgrades that representation in memory so a server
        // upgrade can register new operations without replacing the plaintext
        // token held by a surviving runtime.
        let legacy_grant = serde_json::to_string(&Grant::Session {
            session_id: "s1".to_string(),
            branch_id: branch.id.clone(),
            capabilities: Some(vec!["loom/sessions/read@v1".to_string()]),
        })
        .unwrap();
        sqlx::query("UPDATE api_tokens SET grant_json = ? WHERE bound_session_id = 's1'")
            .bind(legacy_grant)
            .execute(&db)
            .await
            .unwrap();
        let upgraded = lookup_token(&db, &plain).await.unwrap().unwrap();
        assert!(matches!(
            upgraded.grant,
            Grant::Session {
                capabilities: None,
                ..
            }
        ));
        sqlx::query("UPDATE sessions SET policy_restricted = 1 WHERE id = 's1'")
            .execute(&db)
            .await
            .unwrap();
        let pinned = lookup_token(&db, &plain).await.unwrap().unwrap();
        assert!(matches!(
            pinned.grant,
            Grant::Session {
                capabilities: Some(ref values),
                ..
            } if values == &["loom/sessions/read@v1"]
        ));
        assert_eq!(list_tokens(&db, "rjpower").await.unwrap().len(), 0);
        crate::session::set_status(&db, "s1", "archived")
            .await
            .unwrap();
        assert!(lookup_token(&db, &plain).await.unwrap().is_none());
        assert_eq!(revoke_session_tokens(&db, "s1").await.unwrap(), 1);
        assert!(lookup_token(&db, &plain).await.unwrap().is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn expired_token_does_not_resolve() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let (plain, info) = create_token(&db, "rjpower", "old", Some(30)).await.unwrap();
        // Fresh token resolves; once expired, it doesn't.
        assert!(lookup_token(&db, &plain).await.unwrap().is_some());
        sqlx::query("UPDATE api_tokens SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
            .bind(&info.id)
            .execute(&db)
            .await
            .unwrap();
        assert!(lookup_token(&db, &plain).await.unwrap().is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn local_token_is_hidden_and_unrevocable() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let (plain, info) =
            create_token_kind(&db, "rjpower", LOCAL_TOKEN_NAME, None, TokenKind::Local)
                .await
                .unwrap();
        // Authenticates, but never appears in the user-facing list…
        assert!(lookup_token(&db, &plain).await.unwrap().is_some());
        assert!(list_tokens(&db, "rjpower").await.unwrap().is_empty());
        // …and the revoke route can't remove it.
        assert!(!revoke_token(&db, "rjpower", &info.id).await.unwrap());
        assert!(lookup_token(&db, &plain).await.unwrap().is_some());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn sessions_resolve_then_clear_on_logout() {
        let db = connect_in_memory_with_owner("rjpower").await;
        let cookie = create_session(&db, "rjpower").await.unwrap();
        assert_eq!(
            lookup_session(&db, &cookie)
                .await
                .unwrap()
                .map(|p| p.username),
            Some("rjpower".to_string())
        );
        delete_session(&db, &cookie).await.unwrap();
        assert!(lookup_session(&db, &cookie).await.unwrap().is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn password_login_and_user_management() {
        let db = connect_in_memory_with_owner("rjpower").await;
        // The seeded owner has no password yet.
        assert!(verify_login(&db, "rjpower", "x").await.unwrap().is_none());
        set_password(&db, "rjpower", Some("s3cret")).await.unwrap();
        assert!(verify_login(&db, "rjpower", "s3cret")
            .await
            .unwrap()
            .is_some());
        assert!(verify_login(&db, "rjpower", "wrong")
            .await
            .unwrap()
            .is_none());

        add_user(
            &db,
            "alice",
            Some("alice-gh"),
            Some(101),
            None,
            UserRole::User,
        )
        .await
        .unwrap();
        assert_eq!(list_users(&db).await.unwrap().len(), 2);
        assert!(remove_user(&db, "rjpower").await.is_err());
        assert!(remove_user(&db, "alice").await.unwrap());
        // The last remaining administrator can't be removed.
        assert!(remove_user(&db, "rjpower").await.is_err());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn local_token_is_minted_once_and_reused() {
        let db = connect_in_memory_with_owner("rjpower").await;
        // No file is read in-memory; mint goes through the create path twice and
        // each ensures a working bearer for the same owner.
        let first = register_then_lookup(&db).await;
        assert_eq!(first.username, "rjpower");
    }

    async fn register_then_lookup(db: &Db) -> Principal {
        let (plain, _, _) = mint_token();
        register_local_token(db, &plain).await.unwrap();
        // Re-registering the same plaintext is a no-op (conflict ignored).
        register_local_token(db, &plain).await.unwrap();
        lookup_token(db, &plain).await.unwrap().unwrap()
    }
}
