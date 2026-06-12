//! Authentication for the loom daemon — who may drive the fleet over HTTP.
//!
//! This is a loom-only concern: the daemon-less `weaver` CLI talks straight to
//! sqlite and never authenticates. Three credential shapes all resolve to one
//! [`Principal`]:
//!
//! * **API tokens** (`loom_…`) — the `LOOM_TOKEN` a CI job or remote `loom` CLI
//!   sends as `Authorization: Bearer`. Stored hashed; shown once at creation.
//! * **Session cookies** — set after a GitHub or username/password login and
//!   carried by the browser. Stored hashed, same as tokens.
//! * **Loopback trust** — a request from `127.0.0.1`/`::1` is taken to be the
//!   machine owner (the seeded primary user), so the local CLI, the agent, and
//!   overlooker scripts keep working with zero configuration. Gated on the
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

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::db::{weaver_home, Db};

/// Prefix on every loom API token, so a leaked secret is recognisable and
/// greppable. A token looks like `loom_<43 url-safe base64 chars>`.
const TOKEN_PREFIX: &str = "loom_";
/// How much of a token's plaintext is kept (non-secret) for the token list —
/// enough to tell two tokens apart at a glance, far short of guessable.
const PREFIX_KEEP: usize = 12;
/// Browser login lifetime, in days. Shared by the stored-session expiry and the
/// `Max-Age` on the login cookie so the two can't drift.
pub const SESSION_TTL_DAYS: i64 = 30;
/// The cookie a browser login is carried in.
pub const SESSION_COOKIE: &str = "loom_session";
/// The reserved [`TokenKind::Local`] token name.
const LOCAL_TOKEN_NAME: &str = "this machine";
/// SQLite expression for the current instant in our stored ISO format — the one
/// `weaver-core` writes, so string comparisons against `*_at` columns are sound.
const SQL_NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

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
}

impl AuthVia {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthVia::Loopback => "loopback",
            AuthVia::Token => "token",
            AuthVia::Session => "session",
        }
    }
}

/// An authenticated caller: which approved user, and how they proved it.
#[derive(Debug, Clone)]
pub struct Principal {
    pub username: String,
    pub github_login: Option<String>,
    pub via: AuthVia,
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
/// stored hash simply fails (never panics).
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

/// One approved operator.
#[derive(Debug, Clone)]
pub struct User {
    pub username: String,
    pub github_login: Option<String>,
    pub password_hash: Option<String>,
    pub created_at: String,
}

impl User {
    /// Whether this user can log in with a password (has one set).
    pub fn has_password(&self) -> bool {
        self.password_hash.is_some()
    }
}

fn user_from_row(r: &sqlx::sqlite::SqliteRow) -> User {
    User {
        username: r.get("username"),
        github_login: r.get("github_login"),
        password_hash: r.get("password_hash"),
        created_at: r.get("created_at"),
    }
}

pub async fn get_user(db: &Db, username: &str) -> Result<Option<User>> {
    let row = sqlx::query(
        "SELECT username, github_login, password_hash, created_at FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(user_from_row))
}

/// The approved user whose `github_login` matches (case-insensitively — GitHub
/// logins are case-insensitive), if any. This is the allowlist check the OAuth
/// callback runs: an unknown GitHub identity has no row and is rejected.
pub async fn user_by_github(db: &Db, login: &str) -> Result<Option<User>> {
    let row = sqlx::query(
        "SELECT username, github_login, password_hash, created_at FROM users
         WHERE github_login IS NOT NULL AND lower(github_login) = lower(?)",
    )
    .bind(login)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(user_from_row))
}

pub async fn list_users(db: &Db) -> Result<Vec<User>> {
    let rows = sqlx::query(
        "SELECT username, github_login, password_hash, created_at FROM users ORDER BY created_at, username",
    )
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(user_from_row).collect())
}

/// The primary (owner) user: the earliest-created row. Loopback requests and the
/// machine token are attributed to them. `None` only on an unseeded database.
pub async fn primary_user(db: &Db) -> Result<Option<String>> {
    let row = sqlx::query("SELECT username FROM users ORDER BY created_at, username LIMIT 1")
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.get::<String, _>("username")))
}

/// Add an approved user. `github_login` enables GitHub login; `password` (when
/// given) enables password login. At least one should be set or the user can
/// never authenticate, but that is the caller's policy to enforce.
pub async fn add_user(
    db: &Db,
    username: &str,
    github_login: Option<&str>,
    password: Option<&str>,
) -> Result<()> {
    let password_hash = match password {
        Some(p) => Some(hash_password(p)?),
        None => None,
    };
    sqlx::query("INSERT INTO users (username, github_login, password_hash) VALUES (?, ?, ?)")
        .bind(username)
        .bind(github_login)
        .bind(password_hash)
        .execute(db)
        .await
        .with_context(|| format!("adding user '{username}'"))?;
    Ok(())
}

/// Remove an approved user, refusing to delete the last one (which would lock
/// everyone out). Returns whether a row was removed.
pub async fn remove_user(db: &Db, username: &str) -> Result<bool> {
    let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM users")
        .fetch_one(db)
        .await?
        .get("n");
    if count <= 1 {
        return Err(anyhow!("cannot remove the only approved user"));
    }
    let res = sqlx::query("DELETE FROM users WHERE username = ?")
        .bind(username)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Set (or, with `None`, clear) a user's password. Its tokens and sessions are
/// untouched.
pub async fn set_password(db: &Db, username: &str, password: Option<&str>) -> Result<()> {
    let hash = match password {
        Some(p) => Some(hash_password(p)?),
        None => None,
    };
    let res = sqlx::query("UPDATE users SET password_hash = ? WHERE username = ?")
        .bind(hash)
        .bind(username)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(anyhow!("no such user '{username}'"));
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
    let Some(stored) = user.password_hash.as_deref() else {
        return Ok(None);
    };
    if verify_password(password, stored) {
        Ok(Some(Principal {
            username: user.username,
            github_login: user.github_login,
            via: AuthVia::Session,
        }))
    } else {
        Ok(None)
    }
}

/// Build the loopback [`Principal`] — the primary user, marked [`AuthVia::Loopback`].
pub async fn loopback_principal(db: &Db) -> Result<Option<Principal>> {
    let Some(username) = primary_user(db).await? else {
        return Ok(None);
    };
    Ok(get_user(db, &username).await?.map(|u| Principal {
        username: u.username,
        github_login: u.github_login,
        via: AuthVia::Loopback,
    }))
}

// ---------------------------------------------------------------------------
// Browser sessions (login cookies)
// ---------------------------------------------------------------------------

/// Open a browser session for `username`, returning the opaque cookie value.
pub async fn create_session(db: &Db, username: &str) -> Result<String> {
    let (plain, hash, _) = mint_token();
    let sql = format!(
        "INSERT INTO auth_sessions (token_hash, username, expires_at)
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ','now','+{SESSION_TTL_DAYS} days'))"
    );
    sqlx::query(&sql)
        .bind(&hash)
        .bind(username)
        .execute(db)
        .await?;
    Ok(plain)
}

/// Resolve a session cookie to its [`Principal`], or `None` if unknown, expired,
/// or its user has since been removed.
pub async fn lookup_session(db: &Db, cookie: &str) -> Result<Option<Principal>> {
    let hash = sha256_hex(cookie);
    let row = sqlx::query(&format!(
        "SELECT s.username AS username, u.github_login AS github_login
         FROM auth_sessions s JOIN users u ON u.username = s.username
         WHERE s.token_hash = ? AND s.expires_at > {SQL_NOW}"
    ))
    .bind(&hash)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Principal {
        username: r.get("username"),
        github_login: r.get("github_login"),
        via: AuthVia::Session,
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
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

fn token_info_from_row(r: &sqlx::sqlite::SqliteRow) -> TokenInfo {
    TokenInfo {
        id: r.get("id"),
        name: r.get("name"),
        prefix: r.get("prefix"),
        created_at: r.get("created_at"),
        last_used_at: r.get("last_used_at"),
        expires_at: r.get("expires_at"),
    }
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
    let (plain, hash, prefix) = mint_token();
    let id = random_id();
    // Expiry is computed in SQL so it shares the exact stored format; a positive
    // `expires_in_days` sets it, anything else leaves the token non-expiring.
    let expires_sql = match expires_in_days {
        Some(d) if d > 0 => format!("strftime('%Y-%m-%dT%H:%M:%fZ','now','+{d} days')"),
        _ => "NULL".to_string(),
    };
    let sql = format!(
        "INSERT INTO api_tokens (id, username, name, token_hash, prefix, kind, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, {expires_sql})"
    );
    sqlx::query(&sql)
        .bind(&id)
        .bind(username)
        .bind(name)
        .bind(&hash)
        .bind(&prefix)
        .bind(kind.as_str())
        .execute(db)
        .await
        .context("creating token")?;
    let info = get_token(db, &id)
        .await?
        .ok_or_else(|| anyhow!("token vanished after insert"))?;
    Ok((plain, info))
}

async fn get_token(db: &Db, id: &str) -> Result<Option<TokenInfo>> {
    let row = sqlx::query(
        "SELECT id, name, prefix, created_at, last_used_at, expires_at FROM api_tokens WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(token_info_from_row))
}

/// Every user-managed token (the machine 'local' token is infrastructure and is
/// omitted), newest first.
pub async fn list_tokens(db: &Db) -> Result<Vec<TokenInfo>> {
    let rows = sqlx::query(
        "SELECT id, name, prefix, created_at, last_used_at, expires_at FROM api_tokens
         WHERE kind = 'pat' ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(token_info_from_row).collect())
}

/// Revoke a token by id. Refuses the machine 'local' token. Returns whether a
/// (revocable) row was removed.
pub async fn revoke_token(db: &Db, id: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM api_tokens WHERE id = ? AND kind = 'pat'")
        .bind(id)
        .execute(db)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Resolve an `Authorization: Bearer` token to its [`Principal`]. Touches
/// `last_used_at` on a hit (best-effort). `None` for an unknown, expired, or
/// orphaned token.
pub async fn lookup_token(db: &Db, token: &str) -> Result<Option<Principal>> {
    if !token.starts_with(TOKEN_PREFIX) {
        return Ok(None);
    }
    let hash = sha256_hex(token);
    let row = sqlx::query(&format!(
        "SELECT t.id AS id, t.username AS username, u.github_login AS github_login
         FROM api_tokens t JOIN users u ON u.username = t.username
         WHERE t.token_hash = ? AND (t.expires_at IS NULL OR t.expires_at > {SQL_NOW})"
    ))
    .bind(&hash)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: String = row.get("id");
    let _ = sqlx::query(&format!(
        "UPDATE api_tokens SET last_used_at = {SQL_NOW} WHERE id = ?"
    ))
    .bind(&id)
    .execute(db)
    .await;
    Ok(Some(Principal {
        username: row.get("username"),
        github_login: row.get("github_login"),
        via: AuthVia::Token,
    }))
}

// ---------------------------------------------------------------------------
// The machine-local token
// ---------------------------------------------------------------------------

/// Path to the file holding the machine-local token plaintext (mode 0600).
pub fn local_token_path() -> PathBuf {
    weaver_home().join("loom-token")
}

/// Ensure the machine-local bearer token exists and return its plaintext.
///
/// loom injects this into the environments of its own same-host subprocesses
/// (the agent's tmux, overlooker scripts) and the `loom` CLI reads it, so local
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
        "INSERT OR IGNORE INTO api_tokens (id, username, name, token_hash, prefix, kind)
         VALUES (?, ?, ?, ?, ?, 'local')",
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

/// Exchange an OAuth `code` for a GitHub access token.
pub async fn exchange_code(cfg: &GithubOAuth, code: &str, redirect_uri: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        error_description: Option<String>,
    }
    let resp: TokenResp = reqwest::Client::new()
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
        .context("requesting GitHub access token")?
        .json()
        .await
        .context("decoding GitHub token response")?;
    resp.access_token.ok_or_else(|| {
        anyhow!(
            "GitHub did not return an access token: {}",
            resp.error_description.unwrap_or_default()
        )
    })
}

/// Fetch the authenticated user's GitHub login for `access_token`.
pub async fn fetch_github_login(access_token: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct GhUser {
        login: String,
    }
    let user: GhUser = reqwest::Client::new()
        .get("https://api.github.com/user")
        .header(reqwest::header::USER_AGENT, "loom")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .bearer_auth(access_token)
        .send()
        .await
        .context("fetching GitHub user")?
        .error_for_status()
        .context("GitHub user request failed")?
        .json()
        .await
        .context("decoding GitHub user")?;
    Ok(user.login)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

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
        // A garbage stored hash fails closed rather than panicking.
        assert!(!verify_password("hunter2", "not-a-hash"));
    }

    #[tokio::test]
    async fn seeded_owner_is_the_primary_user() {
        let db = db::connect_in_memory().await.unwrap();
        assert_eq!(primary_user(&db).await.unwrap().as_deref(), Some("rjpower"));
        let u = user_by_github(&db, "RJPower").await.unwrap();
        assert_eq!(u.map(|u| u.username), Some("rjpower".to_string()));
    }

    #[tokio::test]
    async fn token_lifecycle_create_lookup_revoke() {
        let db = db::connect_in_memory().await.unwrap();
        let (plain, info) = create_token(&db, "rjpower", "ci", None).await.unwrap();

        let p = lookup_token(&db, &plain)
            .await
            .unwrap()
            .expect("valid token");
        assert_eq!(p.username, "rjpower");
        assert_eq!(p.via, AuthVia::Token);

        assert_eq!(list_tokens(&db).await.unwrap().len(), 1);
        assert!(revoke_token(&db, &info.id).await.unwrap());
        assert!(lookup_token(&db, &plain).await.unwrap().is_none());
        assert!(list_tokens(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn expired_token_does_not_resolve() {
        let db = db::connect_in_memory().await.unwrap();
        let (plain, info) = create_token(&db, "rjpower", "old", Some(30)).await.unwrap();
        // Fresh token resolves; backdate its expiry and it no longer does.
        assert!(lookup_token(&db, &plain).await.unwrap().is_some());
        sqlx::query("UPDATE api_tokens SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
            .bind(&info.id)
            .execute(&db)
            .await
            .unwrap();
        assert!(lookup_token(&db, &plain).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn local_token_is_hidden_and_unrevocable() {
        let db = db::connect_in_memory().await.unwrap();
        let (plain, info) =
            create_token_kind(&db, "rjpower", LOCAL_TOKEN_NAME, None, TokenKind::Local)
                .await
                .unwrap();
        // Authenticates, but never appears in the user-facing list…
        assert!(lookup_token(&db, &plain).await.unwrap().is_some());
        assert!(list_tokens(&db).await.unwrap().is_empty());
        // …and the revoke route can't remove it.
        assert!(!revoke_token(&db, &info.id).await.unwrap());
        assert!(lookup_token(&db, &plain).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn sessions_resolve_then_clear_on_logout() {
        let db = db::connect_in_memory().await.unwrap();
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
    async fn password_login_and_user_management() {
        let db = db::connect_in_memory().await.unwrap();
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

        add_user(&db, "alice", Some("alice-gh"), None)
            .await
            .unwrap();
        assert_eq!(list_users(&db).await.unwrap().len(), 2);
        assert!(remove_user(&db, "alice").await.unwrap());
        // The last remaining user can't be removed.
        assert!(remove_user(&db, "rjpower").await.is_err());
    }

    #[tokio::test]
    async fn local_token_is_minted_once_and_reused() {
        let db = db::connect_in_memory().await.unwrap();
        // No file is read in-memory; mint goes through the create path twice and
        // each ensures a working bearer for the same owner.
        let first = register_then_lookup(&db).await;
        assert_eq!(first.username, "rjpower");
    }

    async fn register_then_lookup(db: &Db) -> Principal {
        let (plain, _, _) = mint_token();
        register_local_token(db, &plain).await.unwrap();
        // Re-registering the same plaintext is a no-op (INSERT OR IGNORE).
        register_local_token(db, &plain).await.unwrap();
        lookup_token(db, &plain).await.unwrap().unwrap()
    }
}
