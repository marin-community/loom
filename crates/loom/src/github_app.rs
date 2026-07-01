//! The GitHub **App** identity (shared-loom design §6.3): the hardening that
//! replaces the v1 webhook's reliance on a long-lived, over-scoped ambient
//! `GH_TOKEN` with **short-lived, least-privilege, per-installation** tokens.
//!
//! An App is configured with an `app_id` and an RSA **private key** (a PEM,
//! held outside the settings registry like the OAuth client secret — never
//! returned by `GET /api/settings`). From those two secrets loom can:
//!
//! 1. **Mint an App JWT** ([`build_app_jwt`]) — an RS256 token, signed with the
//!    private key, that authenticates loom *as the App* for the next ~10 minutes.
//! 2. **Resolve a repo to its installation** ([`GithubApp::installation_id`])
//!    via `GET /repos/{owner}/{name}/installation`. A repo the App is installed
//!    on is authorized when its owner is a trusted owner ([`crate::owners`]) —
//!    the installation *is* the access allowlist, gated on the owner so a public
//!    App can't be driven by a stranger's install (complementing the managed-repo
//!    table from #95).
//! 3. **Exchange the JWT for an installation access token**
//!    (`POST /app/installations/{id}/access_tokens`), **cached per installation
//!    with its expiry** and refreshed once stale ([`GithubApp::installation_token`]).
//!
//! [`GithubApp`] implements the [`GithubApi`] gateway the trigger calls for the
//! commenter permission check and the issue reply, performing both over the
//! **REST API with the installation token** instead of the `gh` CLI. When the
//! App is **not configured** it transparently **falls back** to the ambient
//! `GH_TOKEN` path ([`GhCli`]), so the v1 flow keeps working unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::github_trigger::{GhCli, GithubApi};
use crate::repo::RepoSlug;
use weaver_core::db::Db;

/// Settings key (env-overridable) holding the App's numeric id.
pub const APP_ID_KEY: &str = "github.app_id";
/// Settings key (env-overridable) holding the App's RSA private key PEM. Like
/// the OAuth client secret, this is **never** returned by `GET /api/settings`.
pub const APP_PRIVATE_KEY_KEY: &str = "github.app_private_key";

/// The production GitHub REST base. Overridable per-instance only for tests.
const DEFAULT_API_BASE: &str = "https://api.github.com";
/// GitHub requires a `User-Agent`; identify loom's App client.
const USER_AGENT: &str = "loom-github-app";
const GH_ACCEPT: &str = "application/vnd.github+json";
const GH_API_VERSION: &str = "2022-11-28";

/// JWT validity: GitHub caps an App JWT at 10 minutes; 9 leaves headroom.
const JWT_TTL_SECS: i64 = 9 * 60;
/// Backdate `iat` to tolerate a small clock skew between loom and GitHub.
const CLOCK_SKEW_SECS: i64 = 60;
/// Treat an installation token as stale this long *before* its real expiry, so
/// a token never lapses mid-request.
const TOKEN_EXPIRY_SKEW_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// App JWT — RS256, signed with the App private key.
// ---------------------------------------------------------------------------

/// The registered claims of an App JWT: issued-at, expiry, and the App id as
/// issuer (GitHub accepts the id as a string).
#[derive(Debug, Serialize, Deserialize)]
struct AppJwtClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

/// Build (and RS256-sign) an App JWT for `app_id` using `private_key_pem`,
/// anchored at `now_unix` (seconds since the epoch). Split from the clock so it
/// is deterministic in tests. `iat` is backdated [`CLOCK_SKEW_SECS`] and `exp`
/// is [`JWT_TTL_SECS`] out, both within GitHub's 10-minute ceiling. Errors when
/// the PEM is not a usable RSA private key.
pub fn build_app_jwt(app_id: i64, private_key_pem: &str, now_unix: i64) -> Result<String> {
    let claims = AppJwtClaims {
        iat: now_unix - CLOCK_SKEW_SECS,
        exp: now_unix + JWT_TTL_SECS,
        iss: app_id.to_string(),
    };
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .context("parsing the GitHub App private key (expected an RSA PEM)")?;
    encode(&Header::new(Algorithm::RS256), &claims, &key).context("signing the App JWT")
}

// ---------------------------------------------------------------------------
// Installation token cache.
// ---------------------------------------------------------------------------

/// One installation access token plus the instant it expires.
#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: DateTime<Utc>,
}

impl CachedToken {
    /// Whether this token is still safe to use at `now` (with a refresh margin).
    fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at > now + Duration::seconds(TOKEN_EXPIRY_SKEW_SECS)
    }
}

// ---------------------------------------------------------------------------
// The App client.
// ---------------------------------------------------------------------------

/// loom's GitHub App client: mints App JWTs, exchanges them for per-installation
/// access tokens (cached until expiry), and performs the trigger's GitHub calls
/// over REST with those tokens — falling back to `gh` when the App is
/// unconfigured. One instance per server, shared behind an `Arc`.
pub struct GithubApp {
    db: Db,
    http: reqwest::Client,
    /// REST base (no trailing slash). `https://api.github.com` in production.
    api_base: String,
    /// Per-installation token cache, keyed by installation id.
    tokens: Mutex<HashMap<i64, CachedToken>>,
    /// The ambient-`GH_TOKEN` gateway used when the App is not configured.
    fallback: Arc<dyn GithubApi>,
}

impl GithubApp {
    /// The production client: the real GitHub API, falling back to the `gh` CLI.
    pub fn new(db: Db) -> Self {
        Self::with_parts(db, DEFAULT_API_BASE.to_string(), Arc::new(GhCli))
    }

    /// Construct with an explicit API base and fallback gateway — the seam tests
    /// use to point at a mock GitHub and observe the fallback.
    pub fn with_parts(db: Db, api_base: String, fallback: Arc<dyn GithubApi>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("building the GitHub App HTTP client");
        Self {
            db,
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
            tokens: Mutex::new(HashMap::new()),
            fallback,
        }
    }

    // -- configuration ------------------------------------------------------

    /// The App id: `LOOM_GITHUB_APP_ID`, else the `github.app_id` setting; `None`
    /// when unset or non-numeric.
    pub async fn app_id(&self) -> Option<i64> {
        config_value(&self.db, "LOOM_GITHUB_APP_ID", APP_ID_KEY)
            .await?
            .parse()
            .ok()
    }

    /// The App private key PEM: `LOOM_GITHUB_APP_PRIVATE_KEY`, else the
    /// `github.app_private_key` setting; `None` when unset.
    pub async fn private_key(&self) -> Option<String> {
        config_value(&self.db, "LOOM_GITHUB_APP_PRIVATE_KEY", APP_PRIVATE_KEY_KEY).await
    }

    /// Whether the App is fully configured (both id and private key present). The
    /// switch between the App path and the `gh`-fallback path.
    pub async fn is_configured(&self) -> bool {
        self.app_id().await.is_some() && self.private_key().await.is_some()
    }

    /// A freshly-signed App JWT for the configured App.
    async fn current_jwt(&self) -> Result<String> {
        let app_id = self
            .app_id()
            .await
            .ok_or_else(|| anyhow!("GitHub App id is not configured"))?;
        let pem = self
            .private_key()
            .await
            .ok_or_else(|| anyhow!("GitHub App private key is not configured"))?;
        build_app_jwt(app_id, &pem, Utc::now().timestamp())
    }

    // -- installation resolution + tokens -----------------------------------

    /// The installation id of the App on `owner/name`
    /// (`GET /repos/{owner}/{name}/installation`). Success doubles as the proof
    /// that the App is installed on — and so authorized for — the repo. Errors
    /// (e.g. a 404 when the App is not installed) propagate so callers fail closed.
    pub async fn installation_id(&self, owner: &str, name: &str) -> Result<i64> {
        let jwt = self.current_jwt().await?;
        let url = format!("{}/repos/{owner}/{name}/installation", self.api_base);
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&jwt)
            .send()
            .await
            .context("requesting the repo installation")?;
        let resp = check_status(resp, "resolving the repo installation").await?;
        let body: InstallationResponse = resp
            .json()
            .await
            .context("parsing the installation response")?;
        Ok(body.id)
    }

    /// A valid installation access token for `installation_id`, minting (and
    /// caching) a fresh one only when none is cached or the cached one is near
    /// expiry. The cached-token fast path holds the lock only briefly and never
    /// across the network call.
    pub async fn installation_token(&self, installation_id: i64) -> Result<String> {
        if let Some(token) = self.cached_token(installation_id) {
            return Ok(token);
        }
        let jwt = self.current_jwt().await?;
        let fresh = self.fetch_installation_token(&jwt, installation_id).await?;
        let token = fresh.token.clone();
        self.tokens
            .lock()
            .expect("token cache mutex poisoned")
            .insert(installation_id, fresh);
        Ok(token)
    }

    /// The cached token for `installation_id`, if one is present and still fresh.
    fn cached_token(&self, installation_id: i64) -> Option<String> {
        let now = Utc::now();
        let map = self.tokens.lock().expect("token cache mutex poisoned");
        map.get(&installation_id)
            .filter(|t| t.is_fresh(now))
            .map(|t| t.token.clone())
    }

    /// Exchange `jwt` for an installation access token via
    /// `POST /app/installations/{id}/access_tokens`.
    async fn fetch_installation_token(
        &self,
        jwt: &str,
        installation_id: i64,
    ) -> Result<CachedToken> {
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.api_base
        );
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(jwt)
            .send()
            .await
            .context("requesting an installation access token")?;
        let resp = check_status(resp, "minting an installation token").await?;
        let body: InstallationTokenResponse = resp
            .json()
            .await
            .context("parsing the installation token response")?;
        let expires_at = DateTime::parse_from_rfc3339(&body.expires_at)
            .context("parsing the installation token expiry")?
            .with_timezone(&Utc);
        Ok(CachedToken {
            token: body.token,
            expires_at,
        })
    }

    /// Resolve `owner/name` to an installation and return a valid token for it —
    /// the two-step the REST gateway methods share, and what `crate::repo`
    /// mints a per-clone credential from.
    pub(crate) async fn token_for_repo(&self, owner: &str, name: &str) -> Result<String> {
        let installation_id = self.installation_id(owner, name).await?;
        self.installation_token(installation_id).await
    }

    // -- installation as allowlist ------------------------------------------

    /// When the App is installed on `slug` **and** its owner is a trusted owner
    /// ([`crate::owners`]), ensure that repo is in the managed allowlist
    /// (idempotent), so the trigger's clone path accepts it — the "installation
    /// *is* the allowlist" rule (§6.3), *complementing* the explicitly-registered
    /// repos from #95. The trusted-owner gate is what keeps this safe under a
    /// *public* App: a stranger's installation on their own repo is not honored,
    /// because their account is not on the allowlist. Best-effort and a no-op when
    /// the App is unconfigured, the owner is untrusted, the repo is already
    /// registered, or the App is not installed on it (leaving the v1 repos-table
    /// allowlist to govern).
    pub async fn ensure_installed_repo_registered(&self, slug: &RepoSlug) {
        if !self.is_configured().await {
            return;
        }
        // The trusted-owner gate: honor an installation as a grant only for an
        // owner an operator has allowlisted (see `crate::owners`). This is what
        // makes a *public* App safe — a stranger who installs it on their own
        // repo is not on the list, so their repo is never auto-registered and the
        // clone path (`resolve_clone`) rejects it. Fails closed on a store error.
        match crate::owners::is_allowed(&self.db, &slug.owner).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(
                    owner = %slug.owner,
                    repo = %slug.slug(),
                    "not auto-registering: repo owner is not in the trusted-owner allowlist"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(repo = %slug.slug(), error = %e, "trusted-owner check failed; not auto-registering");
                return;
            }
        }
        let slug_str = slug.slug();
        match crate::repo::get_registered(&self.db, &slug_str).await {
            // Already allowlisted — nothing to do, and no GitHub call needed.
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(repo = %slug_str, error = %e, "checking the repo allowlist failed");
                return;
            }
        }
        // Only auto-register a repo the App is actually installed on.
        let installation_id = match self.installation_id(&slug.owner, &slug.name).await {
            Ok(id) => id,
            Err(e) => {
                tracing::debug!(repo = %slug_str, error = %e, "repo has no App installation; not auto-registering");
                return;
            }
        };
        let path = slug.path(&crate::repo::repos_dir());
        match crate::repo::register(
            &self.db,
            &slug_str,
            &slug.github_url(),
            &path.to_string_lossy(),
        )
        .await
        {
            Ok(_) => tracing::info!(
                repo = %slug_str,
                installation = installation_id,
                "auto-registered an App-installed repo into the managed allowlist"
            ),
            Err(e) => {
                tracing::warn!(repo = %slug_str, error = %e, "auto-registering the installed repo failed")
            }
        }
    }
}

#[async_trait::async_trait]
impl GithubApi for GithubApp {
    async fn collaborator_permission(
        &self,
        owner: &str,
        name: &str,
        login: &str,
    ) -> Result<String> {
        if !self.is_configured().await {
            return self
                .fallback
                .collaborator_permission(owner, name, login)
                .await;
        }
        let token = self.token_for_repo(owner, name).await?;
        let url = format!(
            "{}/repos/{owner}/{name}/collaborators/{login}/permission",
            self.api_base
        );
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&token)
            .send()
            .await
            .context("requesting the collaborator permission")?;
        let resp = check_status(resp, "checking the collaborator permission").await?;
        let body: PermissionResponse = resp
            .json()
            .await
            .context("parsing the collaborator permission response")?;
        Ok(body.permission)
    }

    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<()> {
        if !self.is_configured().await {
            return self.fallback.post_issue_comment(repo, issue, body).await;
        }
        let slug = crate::repo::parse_slug(repo).map_err(|e| anyhow!(e))?;
        let token = self.token_for_repo(&slug.owner, &slug.name).await?;
        let url = format!(
            "{}/repos/{}/{}/issues/{issue}/comments",
            self.api_base, slug.owner, slug.name
        );
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .context("posting the issue comment")?;
        check_status(resp, "posting the issue comment").await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// REST response shapes + helpers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InstallationResponse {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    /// RFC 3339, e.g. `2016-07-11T22:14:10Z`.
    expires_at: String,
}

#[derive(Debug, Deserialize)]
struct PermissionResponse {
    permission: String,
}

/// `env`, else the `key` setting; `None` when neither holds a non-empty value.
/// Mirrors the OAuth-secret resolution in [`crate::auth`].
async fn config_value(db: &Db, env: &str, key: &str) -> Option<String> {
    if let Ok(v) = std::env::var(env) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let v = weaver_core::config::get(db, key).await?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Turn a non-2xx GitHub response into an error carrying the (trimmed) body, so
/// the caller can log and fail closed; pass a 2xx response through untouched.
async fn check_status(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    bail!("{what}: GitHub returned {status}: {}", body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::{Json, Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use jsonwebtoken::{decode, DecodingKey, Validation};
    use serde_json::{json, Value};

    /// A throwaway RSA keypair (PKCS#8 PEM) used only by these tests to sign and
    /// verify App JWTs. Never a real credential.
    const TEST_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDaaguBS++TNGq8
erDndgGPa553S+MqsFQ2E8mAadAaEsDSZSz7PqrLbdf8zGBGL7Ehzye2acTPt6Fo
3z5AmazjTN6RcdAzWoq7q990extzliW9mKv9PY88olpTB7VTUPDBtRaKghcgVRKY
m014jt/1cbZ97nOZHa9wmExP7R98PC1ewZ3qYbXL+i1T6zIFDzfB3aY6px1VI9qJ
umDzPojzg0Lg9g1We7j8l+J7zngPhoEwRuwjCw1EiCTWyp6GqhHkOvqkFOyd/1C5
PQI3x9JS4HkL0OMXzumDdizIuuqyda3rikufWUJ7qhy9pIanR4vYYCLb8RZMCnz5
cXRmeogLAgMBAAECggEAWFRaosea8+VW5TKZKIJIzz+uroA6NqFo7RXDf/NK/cBn
yq6wKkuFtw+NMedVaA0RjaLBZLwRpA+Xb1oZSvbbPHFx8VAd6ybKxGsVy32d9Hjc
enir1ZZ3vwXJkZqkcjVhqHUb0Jgb0i+VfbIQ+piNai26p+MvTNT8hoSRGCHFge/1
AlDGufKPaMqE91BYUdBN8eGjTJlaVbLJM2XQxLEtcNhfBHaGgFuqWHJJuYoXX2IU
EbG9jjVzX8zx6Kp8rF8k4H0Y6LzhTjEEbBIHkUQqv5PS6qajDOSeT3pSF0Hb5CWL
LQ/gG9Y9ttN/D2lOd3IiATU6hnBfjHqO4YhleU91gQKBgQDvvaGZxToIStgEX+bS
+jhriUiHgw4xCv+kz5eok0thl/fnsM5aKZDeMEvzhNoBZotWEu18xKC8rfV1orj7
LDTH3RN1N3AY06ebYb2DWX9sqe+7P/Y/T6C/b0/R2/yP1vGkUgRpUIUZi7wy9Mkc
4qd7KYalYplFSxXU8Y3uocJLgwKBgQDpOiTMBRMwD4aiZ/M4doyenByuYbQ1yKmX
QtaRVBvHIp3TTGIC2l7j4FeoYQL7Uh02NI1KlpACB54uakVbu9xHotZLpXWsCxLV
l5Io/OxnyZANA6EEfdk2U0ZAyrS+K6He67XR0DJqfd15jvowCgJXH0lrlsFjdp6e
81dwbNCC2QKBgHiXxOAaq3RcYYjhzLQ3lYXSSp+PtuXIiIuYuMrdPL/ct6Dd+Q61
dd+uH6ZhH2Aw+snTP47RQaFnR99iePYvaGVYuV7vAf4bCWZJphCaRlScrrBcHjv+
i/d/wIDpzYN1NZvYfcuT6z/MYGCpbTiQcnqrisVKcZq/iD3TO/fbemaNAoGBAJKL
STG0gqDxMHx9anLw8lx65P6hP5WH1x/HDIFWYvnWA2sQFImMYpE2ln2jLzdxGg/E
J39VaXkNBlRNy/Te7oNIivQPLAgFETmKOnlsqrJwEQZMYHEtDj23R25QsA7J5bTn
UGBcPEFzgqTttMBYma3aZ8yldjAkCXkAl9F5Xe7JAoGAStxmRJTtYWpyU84wFvWA
Tm6lfFMOJWNssPbj8PaqekEG8CALxgG4C/KNWbB0sO6OSs3U1ihEQINSbbX0Y15F
hlbeI/D+Z+U3ASALKlIsZTEuz+5fTKByEa0bezukkMPD0GJU6vj4ik6MIwEZJVWG
8MEpYrwHIf1vyElxCHpJAqs=
-----END PRIVATE KEY-----";

    const TEST_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA2moLgUvvkzRqvHqw53YB
j2ued0vjKrBUNhPJgGnQGhLA0mUs+z6qy23X/MxgRi+xIc8ntmnEz7ehaN8+QJms
40zekXHQM1qKu6vfdHsbc5YlvZir/T2PPKJaUwe1U1DwwbUWioIXIFUSmJtNeI7f
9XG2fe5zmR2vcJhMT+0ffDwtXsGd6mG1y/otU+syBQ83wd2mOqcdVSPaibpg8z6I
84NC4PYNVnu4/Jfie854D4aBMEbsIwsNRIgk1sqehqoR5Dr6pBTsnf9QuT0CN8fS
UuB5C9DjF87pg3YsyLrqsnWt64pLn1lCe6ocvaSGp0eL2GAi2/EWTAp8+XF0ZnqI
CwIDAQAB
-----END PUBLIC KEY-----";

    const TEST_APP_ID: i64 = 123456;

    // -- JWT ----------------------------------------------------------------

    /// The minted App JWT verifies against the public key and carries the
    /// expected issuer and a sane iat/exp window (RS256, ≤10-minute TTL).
    #[test]
    fn app_jwt_is_well_formed_and_verifiable() {
        // Anchor at the real clock so the freshly-minted token is unexpired and
        // `validate_exp` (below) genuinely passes.
        let now = Utc::now().timestamp();
        let token = build_app_jwt(TEST_APP_ID, TEST_PRIVATE_KEY, now).unwrap();

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[TEST_APP_ID.to_string()]);
        // Anchor expiry validation to the token's own clock window.
        validation.validate_exp = true;
        validation.leeway = 0;
        let key = DecodingKey::from_rsa_pem(TEST_PUBLIC_KEY.as_bytes()).unwrap();
        let data = decode::<AppJwtClaims>(&token, &key, &validation).unwrap();

        assert_eq!(data.claims.iss, TEST_APP_ID.to_string());
        // iat is backdated for skew; exp is within GitHub's 10-minute ceiling.
        assert_eq!(data.claims.iat, now - CLOCK_SKEW_SECS);
        assert_eq!(data.claims.exp, now + JWT_TTL_SECS);
        assert!(data.claims.exp - data.claims.iat <= 600);
    }

    /// A wrong public key (a tampered-with or mismatched App) fails verification.
    #[test]
    fn app_jwt_rejects_a_mismatched_key() {
        let token = build_app_jwt(TEST_APP_ID, TEST_PRIVATE_KEY, 1_700_000_000).unwrap();
        let other = "-----BEGIN PUBLIC KEY-----
MFwwDQYJKoZIhvcNAQEBBQADSwAwSAJBALB1n9OQb2v0gQ0F0G0t0Q0G0t0Q0G0t
0Q0G0t0Q0G0t0Q0G0t0Q0G0t0Q0G0t0Q0G0t0Q0G0t0Q0G0t0CAwEAAQ==
-----END PUBLIC KEY-----";
        let validation = Validation::new(Algorithm::RS256);
        // A malformed/mismatched key must not verify (either parse or verify fails).
        let verified = DecodingKey::from_rsa_pem(other.as_bytes())
            .ok()
            .and_then(|k| decode::<AppJwtClaims>(&token, &k, &validation).ok());
        assert!(verified.is_none());
    }

    #[test]
    fn app_jwt_rejects_a_bad_private_key() {
        assert!(build_app_jwt(TEST_APP_ID, "not a pem", 0).is_err());
    }

    // -- mock GitHub --------------------------------------------------------

    /// Shared state for the mock GitHub server: how many tokens it has minted,
    /// the expiry it stamps on them, and the comments it received.
    struct MockState {
        token_mints: AtomicUsize,
        /// Offset from now stamped as the token's `expires_at` (seconds). Negative
        /// → already expired, to exercise refresh.
        expiry_offset_secs: i64,
        comments: Mutex<Vec<Value>>,
        last_comment_auth: Mutex<Option<String>>,
    }

    impl MockState {
        fn new(expiry_offset_secs: i64) -> Arc<Self> {
            Arc::new(Self {
                token_mints: AtomicUsize::new(0),
                expiry_offset_secs,
                comments: Mutex::new(Vec::new()),
                last_comment_auth: Mutex::new(None),
            })
        }
    }

    async fn mock_access_tokens(State(s): State<Arc<MockState>>) -> Json<Value> {
        s.token_mints.fetch_add(1, Ordering::SeqCst);
        let exp = Utc::now() + Duration::seconds(s.expiry_offset_secs);
        Json(json!({ "token": "ghs_installation_token", "expires_at": exp.to_rfc3339() }))
    }

    async fn mock_installation() -> Json<Value> {
        Json(json!({ "id": 42 }))
    }

    async fn mock_permission() -> Json<Value> {
        Json(json!({ "permission": "write" }))
    }

    async fn mock_comments(
        State(s): State<Arc<MockState>>,
        Path((owner, name, issue)): Path<(String, String, i64)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> StatusCode {
        *s.last_comment_auth.lock().unwrap() = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        s.comments.lock().unwrap().push(json!({
            "repo": format!("{owner}/{name}"),
            "issue": issue,
            "body": body["body"],
        }));
        StatusCode::CREATED
    }

    /// Spawn the mock GitHub REST server on a random port; returns its base URL.
    async fn spawn_mock(state: Arc<MockState>) -> String {
        let app = Router::new()
            .route(
                "/app/installations/{id}/access_tokens",
                post(mock_access_tokens),
            )
            .route("/repos/{owner}/{name}/installation", get(mock_installation))
            .route(
                "/repos/{owner}/{name}/collaborators/{login}/permission",
                get(mock_permission),
            )
            .route(
                "/repos/{owner}/{name}/issues/{issue}/comments",
                post(mock_comments),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// A recording fallback gateway, standing in for the `gh`/`GH_TOKEN` path, to
    /// prove the App delegates to it when unconfigured.
    #[derive(Default)]
    struct RecordingFallback {
        permission_calls: Mutex<Vec<(String, String, String)>>,
        comment_calls: Mutex<Vec<(String, i64, String)>>,
    }

    #[async_trait::async_trait]
    impl GithubApi for RecordingFallback {
        async fn collaborator_permission(&self, o: &str, n: &str, l: &str) -> Result<String> {
            self.permission_calls.lock().unwrap().push((
                o.to_string(),
                n.to_string(),
                l.to_string(),
            ));
            Ok("admin".to_string())
        }
        async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<()> {
            self.comment_calls
                .lock()
                .unwrap()
                .push((repo.to_string(), issue, body.to_string()));
            Ok(())
        }
    }

    /// A configured `GithubApp` pointed at `api_base`, with the App id + test
    /// private key written to its (in-memory) settings — never via env, so unit
    /// tests stay parallel-safe.
    async fn configured_app(api_base: String, fallback: Arc<dyn GithubApi>) -> GithubApp {
        let db = crate::db::connect_in_memory().await.unwrap();
        weaver_core::config::apply(
            &db,
            &[
                (APP_ID_KEY.to_string(), Some(TEST_APP_ID.to_string())),
                (
                    APP_PRIVATE_KEY_KEY.to_string(),
                    Some(TEST_PRIVATE_KEY.to_string()),
                ),
            ],
        )
        .await
        .unwrap();
        GithubApp::with_parts(db, api_base, fallback)
    }

    // -- installation token exchange + caching ------------------------------

    /// A minted token is reused while fresh: a second request inside its lifetime
    /// hits the cache, not the GitHub token endpoint.
    #[tokio::test]
    async fn installation_token_is_cached_while_fresh() {
        let mock = MockState::new(3600); // expires an hour out
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base, Arc::new(RecordingFallback::default())).await;

        let t1 = app.installation_token(42).await.unwrap();
        let t2 = app.installation_token(42).await.unwrap();
        assert_eq!(t1, "ghs_installation_token");
        assert_eq!(t1, t2);
        assert_eq!(
            mock.token_mints.load(Ordering::SeqCst),
            1,
            "the second call reused the cached token"
        );
    }

    /// A token at/after its expiry is never served from cache: each request
    /// re-mints, so an expired token is refreshed rather than reused.
    #[tokio::test]
    async fn installation_token_refreshes_once_expired() {
        let mock = MockState::new(-10); // already expired (inside the skew window)
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base, Arc::new(RecordingFallback::default())).await;

        app.installation_token(42).await.unwrap();
        app.installation_token(42).await.unwrap();
        assert_eq!(
            mock.token_mints.load(Ordering::SeqCst),
            2,
            "an expired token is re-minted, not reused"
        );
    }

    // -- REST gateway calls -------------------------------------------------

    /// The permission check goes over REST with an installation token (resolve
    /// installation → mint token → GET permission).
    #[tokio::test]
    async fn collaborator_permission_uses_rest() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base, Arc::new(RecordingFallback::default())).await;

        let perm = app
            .collaborator_permission("acme", "widgets", "octocat")
            .await
            .unwrap();
        assert_eq!(perm, "write");
        assert_eq!(
            mock.token_mints.load(Ordering::SeqCst),
            1,
            "an installation token was minted for the call"
        );
    }

    /// The reply posts over REST, authenticated with the installation token.
    #[tokio::test]
    async fn post_issue_comment_uses_installation_token() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base, Arc::new(RecordingFallback::default())).await;

        app.post_issue_comment("acme/widgets", 7, "On it — http://loom/s/abc")
            .await
            .unwrap();

        let comments = mock.comments.lock().unwrap().clone();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["repo"], "acme/widgets");
        assert_eq!(comments[0]["issue"], 7);
        assert_eq!(comments[0]["body"], "On it — http://loom/s/abc");
        // The request carried the minted installation token, not the App JWT.
        assert_eq!(
            mock.last_comment_auth.lock().unwrap().clone(),
            Some("Bearer ghs_installation_token".to_string()),
        );
    }

    // -- installation as the allowlist (§6.3) -------------------------------

    /// A repo the App is installed on, *whose owner is a trusted owner*, is
    /// auto-registered into the managed allowlist so the trigger may clone it —
    /// the "installation is the allowlist" rule, complementing the
    /// explicitly-registered repos.
    #[tokio::test]
    async fn installed_repo_is_auto_registered() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base, Arc::new(RecordingFallback::default())).await;
        crate::owners::add(&app.db, "acme").await.unwrap();

        let slug = crate::repo::parse_slug("acme/widgets").unwrap();
        assert!(
            crate::repo::get_registered(&app.db, "acme/widgets")
                .await
                .unwrap()
                .is_none(),
            "not registered to begin with"
        );

        app.ensure_installed_repo_registered(&slug).await;

        let registered = crate::repo::get_registered(&app.db, "acme/widgets")
            .await
            .unwrap()
            .expect("the installed repo was added to the allowlist");
        assert_eq!(registered.slug, "acme/widgets");
        assert_eq!(registered.remote_url, "https://github.com/acme/widgets.git");
    }

    /// The trusted-owner gate: a repo the App is installed on is **not**
    /// auto-registered when its owner is not on the allowlist — the protection
    /// that keeps a public App from honoring a stranger's installation.
    #[tokio::test]
    async fn installed_repo_with_untrusted_owner_is_not_registered() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base, Arc::new(RecordingFallback::default())).await;
        // Note: "stranger" is deliberately NOT added to the owner allowlist.

        let slug = crate::repo::parse_slug("stranger/evil").unwrap();
        app.ensure_installed_repo_registered(&slug).await;

        assert!(
            crate::repo::get_registered(&app.db, "stranger/evil")
                .await
                .unwrap()
                .is_none(),
            "an untrusted owner's repo must not be auto-registered"
        );
    }

    /// When the App is unconfigured the auto-register step is inert: the v1
    /// repos-table allowlist alone governs.
    #[tokio::test]
    async fn unconfigured_app_does_not_auto_register() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let db = crate::db::connect_in_memory().await.unwrap();
        let app = GithubApp::with_parts(db, base, Arc::new(RecordingFallback::default()));

        let slug = crate::repo::parse_slug("acme/widgets").unwrap();
        app.ensure_installed_repo_registered(&slug).await;

        assert!(
            crate::repo::get_registered(&app.db, "acme/widgets")
                .await
                .unwrap()
                .is_none(),
            "an unconfigured App registers nothing"
        );
        assert_eq!(mock.token_mints.load(Ordering::SeqCst), 0);
    }

    // -- fallback when unconfigured -----------------------------------------

    /// With no App configured, the gateway falls back to the ambient-`GH_TOKEN`
    /// path (`gh`) and never touches the REST API.
    #[tokio::test]
    async fn falls_back_to_gh_when_unconfigured() {
        // A mock that would record a mint if (wrongly) reached.
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let fallback = Arc::new(RecordingFallback::default());
        // An empty db and no env → not configured.
        let db = crate::db::connect_in_memory().await.unwrap();
        let app = GithubApp::with_parts(db, base, fallback.clone());

        assert!(!app.is_configured().await);

        let perm = app
            .collaborator_permission("acme", "widgets", "octocat")
            .await
            .unwrap();
        app.post_issue_comment("acme/widgets", 7, "hi")
            .await
            .unwrap();

        // The fallback handled both calls…
        assert_eq!(perm, "admin");
        assert_eq!(fallback.permission_calls.lock().unwrap().len(), 1);
        assert_eq!(fallback.comment_calls.lock().unwrap().len(), 1);
        // …and the REST API was never reached.
        assert_eq!(mock.token_mints.load(Ordering::SeqCst), 0);
        assert!(mock.comments.lock().unwrap().is_empty());
    }
}
