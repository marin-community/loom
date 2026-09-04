//! Loom's GitHub **App** identity (shared-loom design §6.3), which provides
//! short-lived, least-privilege, per-installation tokens.
//!
//! An App is configured with an `app_id` and an RSA **private key** (a PEM,
//! held outside the settings registry like the OAuth client secret — never
//! returned by `settings.get`). From those two secrets loom can:
//!
//! 1. **Mint an App JWT** ([`build_app_jwt`]) — an RS256 token, signed with the
//!    private key, that authenticates loom *as the App* for the next ~10 minutes.
//! 2. **Resolve a repo to its installation** ([`GithubApp::installation_id`])
//!    via `GET /repos/{owner}/{name}/installation`. A repo the App is installed
//!    on is authorized when its owner is a trusted owner ([`crate::owners`]) —
//!    the installation *is* the access allowlist, gated on the owner so a public
//!    App can't be driven by a stranger's install (complementing the managed-repo
//!    table).
//! 3. **Exchange the JWT for an installation access token**
//!    (`POST /app/installations/{id}/access_tokens`), **cached by exact scope
//!    with its expiry** and refreshed once stale ([`GithubApp::installation_token`]).
//!
//! [`GithubApp`] implements the [`GithubApi`] gateway the trigger calls for the
//! commenter permission check and the issue reply, performing both over the
//! **REST API with the installation token**. App-backed operations require the
//! App id and private key to be configured.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::github_trigger::{GithubApi, PrHead};
use crate::repo::RepoSlug;
use weaver_core::config::GithubOrganization;
use weaver_core::db::Db;

/// Settings key (env-overridable) holding the App's numeric id.
pub const APP_ID_KEY: &str = "github.app_id";
/// Settings key (env-overridable) holding the App's RSA private key PEM. Like
/// the OAuth client secret, this is **never** returned by `settings.get`.
pub const APP_PRIVATE_KEY_KEY: &str = "github.app_private_key";
/// Settings key (env-overridable) holding the App's URL slug (e.g. `loom-acme`,
/// from the manifest conversion). Public and non-secret. Read at runtime via
/// [`app_slug`] so the settings view can name the App and link to
/// `github.com/apps/{slug}`, and recorded so `loom setup` can deep-link to the
/// App's GitHub settings/install pages when updating an already-configured App.
pub const APP_SLUG_KEY: &str = "github.app_slug";
/// Settings key holding the org login that owns the App, when it's an org-owned
/// App (empty for a personal App). Together with [`APP_SLUG_KEY`] it picks the
/// right (org vs personal) GitHub settings URL for the update flow.
pub const APP_OWNER_KEY: &str = "github.app_owner";

/// The production GitHub REST base. Overridable per-instance only for tests.
const DEFAULT_API_BASE: &str = "https://api.github.com";
/// GitHub requires a `User-Agent`; identify loom's App client.
const USER_AGENT: &str = "loom-github-app";
const GH_ACCEPT: &str = "application/vnd.github+json";
const GH_API_VERSION: &str = "2022-11-28";
/// Bound every GitHub App API call so an authorization check fails closed
/// promptly when GitHub is unavailable.
const REQUEST_TIMEOUT_SECS: u64 = 10;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TokenScope {
    Installation(i64),
    Repositories(Vec<String>),
}

impl CachedToken {
    /// Whether this token is still safe to use at `now` (with a refresh margin).
    fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at > now + Duration::seconds(TOKEN_EXPIRY_SKEW_SECS)
    }
}

// ---------------------------------------------------------------------------
// Configuration resolution (env-or-setting).
//
// These `db`-only resolvers are the single source of truth for "is the App
// configured, and what is its public identity" — shared by the [`GithubApp`]
// client (via the delegating methods below) and the settings view
// ([`crate::web`]), so both agree on what the trigger will actually use.
// ---------------------------------------------------------------------------

/// The App id from `LOOM_GITHUB_APP_ID`, else the `github.app_id` setting;
/// `None` when unset or non-numeric.
pub async fn app_id(db: &Db) -> Option<i64> {
    config_value(db, "LOOM_GITHUB_APP_ID", APP_ID_KEY)
        .await?
        .parse()
        .ok()
}

/// The App private key PEM from `LOOM_GITHUB_APP_PRIVATE_KEY`, else the
/// `github.app_private_key` setting; `None` when unset.
pub async fn private_key(db: &Db) -> Option<String> {
    config_value(db, "LOOM_GITHUB_APP_PRIVATE_KEY", APP_PRIVATE_KEY_KEY).await
}

/// The App slug from `LOOM_GITHUB_APP_SLUG`, else the `github.app_slug` setting;
/// `None` when unset. Only `loom setup github-app` records it, so a
/// hand-configured App may have the id and key set but no slug — callers fall
/// back to the id.
pub async fn app_slug(db: &Db) -> Option<String> {
    config_value(db, "LOOM_GITHUB_APP_SLUG", APP_SLUG_KEY).await
}

/// Whether the App is fully configured (both id and private key present).
pub async fn is_configured(db: &Db) -> bool {
    app_id(db).await.is_some() && private_key(db).await.is_some()
}

// ---------------------------------------------------------------------------
// The App client.
// ---------------------------------------------------------------------------

/// loom's GitHub App client: mints App JWTs, exchanges them for per-installation
/// access tokens (cached until expiry), and performs the trigger's GitHub calls
/// over REST with those tokens. One instance per server, shared behind an `Arc`.
pub struct GithubApp {
    db: Db,
    http: reqwest::Client,
    /// REST base (no trailing slash). `https://api.github.com` in production.
    api_base: String,
    /// Installation tokens cached by their exact repository scope.
    tokens: Mutex<HashMap<TokenScope, CachedToken>>,
}

/// The two GitHub thread resources exposed by restricted-session tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubThreadKind {
    Issue,
    PullRequest,
}

/// An active organization membership verified against both immutable GitHub
/// account ids. The current login is returned only as display/routing data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOrganizationMembership {
    pub github_login: String,
    pub organization: GithubOrganization,
}

impl GithubThreadKind {
    fn resource(self) -> &'static str {
        match self {
            Self::Issue => "issues",
            Self::PullRequest => "pulls",
        }
    }
}

impl GithubApp {
    /// The production client for the real GitHub API.
    pub fn new(db: Db) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .expect("building the GitHub App HTTP client");
        Self {
            db,
            http,
            api_base: DEFAULT_API_BASE.to_string(),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    /// Construct with an explicit API base for mock GitHub tests.
    #[cfg(any(test, feature = "test-support"))]
    fn with_parts(db: Db, api_base: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .expect("building the GitHub App HTTP client");
        Self {
            db,
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    // -- configuration ------------------------------------------------------

    /// The App id: `LOOM_GITHUB_APP_ID`, else the `github.app_id` setting; `None`
    /// when unset or non-numeric.
    pub async fn app_id(&self) -> Option<i64> {
        app_id(&self.db).await
    }

    /// The App private key PEM: `LOOM_GITHUB_APP_PRIVATE_KEY`, else the
    /// `github.app_private_key` setting; `None` when unset.
    pub async fn private_key(&self) -> Option<String> {
        private_key(&self.db).await
    }

    /// Whether the App is fully configured (both id and private key present).
    pub async fn is_configured(&self) -> bool {
        is_configured(&self.db).await
    }

    /// Execute a GraphQL query as the App installation for `repo`.
    pub async fn graphql(&self, repo: &RepoSlug, query: &str) -> Result<Vec<u8>> {
        let token = self.token_for_repo(&repo.owner, &repo.name).await?;
        let response = self
            .http
            .post(format!("{}/graphql", self.api_base))
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(token)
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await
            .context("executing GitHub GraphQL query")?;
        let response = check_status(response, "executing GitHub GraphQL query").await?;
        Ok(response
            .bytes()
            .await
            .context("reading GitHub GraphQL response")?
            .to_vec())
    }

    async fn repo_request(
        &self,
        repo: &RepoSlug,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
        action: &str,
    ) -> Result<reqwest::Response> {
        let token = self.token_for_repo(&repo.owner, &repo.name).await?;
        let url = format!(
            "{}/repos/{}/{}/{}",
            self.api_base,
            repo.owner,
            repo.name,
            path.trim_start_matches('/')
        );
        let mut request = self
            .http
            .request(method, url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("{action} on GitHub"))?;
        check_status(response, action).await
    }

    /// Fetch one issue or pull request for a restricted session and return the
    /// stable, bounded field set exposed by the MCP tool.
    pub async fn thread_view(
        &self,
        repo: &RepoSlug,
        kind: GithubThreadKind,
        number: i64,
    ) -> Result<serde_json::Value> {
        let response = self
            .repo_request(
                repo,
                reqwest::Method::GET,
                &format!("{}/{number}", kind.resource()),
                None,
                "fetching the GitHub thread",
            )
            .await?;
        let value: serde_json::Value = response
            .json()
            .await
            .context("parsing the GitHub thread response")?;
        Ok(serde_json::json!({
            "number": value["number"],
            "title": value["title"],
            "body": value["body"],
            "url": value["html_url"],
            "state": value["state"],
        }))
    }

    /// Add a comment to an issue or pull request and return its id. GitHub
    /// exposes comments for both through the issue-comment endpoint.
    pub async fn thread_comment(&self, repo: &RepoSlug, number: i64, body: &str) -> Result<i64> {
        let response = self
            .repo_request(
                repo,
                reqwest::Method::POST,
                &format!("issues/{number}/comments"),
                Some(&serde_json::json!({ "body": body })),
                "commenting on the GitHub thread",
            )
            .await?;
        let created: serde_json::Value = response
            .json()
            .await
            .context("parsing created-comment json")?;
        created["id"]
            .as_i64()
            .ok_or_else(|| anyhow!("created-comment json carries no id"))
    }

    /// Edit the body and optional title of an issue or pull request.
    pub async fn thread_edit(
        &self,
        repo: &RepoSlug,
        kind: GithubThreadKind,
        number: i64,
        title: Option<&str>,
        body: &str,
    ) -> Result<()> {
        let mut payload = serde_json::json!({ "body": body });
        if let Some(title) = title {
            payload["title"] = serde_json::Value::String(title.to_string());
        }
        self.repo_request(
            repo,
            reqwest::Method::PATCH,
            &format!("{}/{number}", kind.resource()),
            Some(&payload),
            "editing the GitHub thread",
        )
        .await?;
        Ok(())
    }

    /// Add labels to an issue or pull request. GitHub exposes PR labels through
    /// the issue endpoint, so the caller only needs the thread number.
    pub async fn add_thread_labels(
        &self,
        repo: &RepoSlug,
        number: i64,
        labels: &[String],
    ) -> Result<()> {
        self.repo_request(
            repo,
            reqwest::Method::POST,
            &format!("issues/{number}/labels"),
            Some(&serde_json::json!({ "labels": labels })),
            "labelling the GitHub thread",
        )
        .await?;
        Ok(())
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
        tracing::debug!(app_id, "minting app jwt");
        build_app_jwt(app_id, &pem, Utc::now().timestamp())
    }

    fn api_url<'a>(&self, segments: impl IntoIterator<Item = &'a str>) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.api_base).context("parsing GitHub API base URL")?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("GitHub API base URL cannot contain path segments"))?
            .extend(segments);
        Ok(url)
    }

    // -- installation resolution + tokens -----------------------------------

    /// The installation id of the App on `owner/name`
    /// (`GET /repos/{owner}/{name}/installation`). Success doubles as the proof
    /// that the App is installed on — and so authorized for — the repo. Errors
    /// (e.g. a 404 when the App is not installed) propagate so callers fail closed.
    pub async fn installation_id(&self, owner: &str, name: &str) -> Result<i64> {
        tracing::debug!(owner, name, "resolving repo installation");
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
        tracing::debug!(
            owner,
            name,
            installation = body.id,
            "resolved repo installation"
        );
        Ok(body.id)
    }

    /// A valid installation access token for `installation_id`, minting (and
    /// caching) a fresh one only when none is cached or the cached one is near
    /// expiry. The cached-token fast path holds the lock only briefly and never
    /// across the network call.
    pub async fn installation_token(&self, installation_id: i64) -> Result<String> {
        let scope = TokenScope::Installation(installation_id);
        if let Some(token) = self.cached_token(&scope) {
            return Ok(token);
        }
        let jwt = self.current_jwt().await?;
        let fresh = self
            .fetch_installation_token(&jwt, installation_id, &[])
            .await?;
        tracing::info!(
            installation = installation_id,
            expires_at = %fresh.expires_at,
            "minted installation access token"
        );
        let token = fresh.token.clone();
        self.tokens
            .lock()
            .expect("token cache mutex poisoned")
            .insert(scope, fresh);
        Ok(token)
    }

    /// Check all configured organizations with their App installations. Active
    /// membership in any one organization wins over inactive or failed checks;
    /// without an active result, any failed check makes the aggregate fail
    /// closed instead of being mistaken for definitive non-membership.
    pub async fn active_organization_membership(
        &self,
        organizations: &[GithubOrganization],
        github_user_id: i64,
    ) -> Result<Option<VerifiedOrganizationMembership>> {
        let mut indeterminate = Vec::new();
        for organization in organizations {
            match self
                .organization_membership(organization, github_user_id)
                .await
            {
                Ok(Some(github_login)) => {
                    return Ok(Some(VerifiedOrganizationMembership {
                        github_login,
                        organization: organization.clone(),
                    }));
                }
                Ok(None) => {}
                Err(error) => indeterminate.push(format!("{}: {error}", organization.login)),
            }
        }
        if indeterminate.is_empty() {
            return Ok(None);
        }
        bail!(
            "GitHub organization membership could not be verified: {}",
            indeterminate.join("; ")
        )
    }

    /// Resolve the current login for `github_user_id`, then check one active
    /// organization membership. Both response objects must repeat the expected
    /// numeric user id, preventing a reclaimed login from renewing another
    /// account's authorization.
    async fn organization_membership(
        &self,
        organization: &GithubOrganization,
        github_user_id: i64,
    ) -> Result<Option<String>> {
        let jwt = self.current_jwt().await?;
        let installation = self
            .http
            .get(format!(
                "{}/orgs/{}/installation",
                self.api_base, organization.login
            ))
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(jwt)
            .send()
            .await
            .context("requesting the organization installation")?;
        let installation = check_status(installation, "resolving the organization installation")
            .await?
            .json::<OrganizationInstallationResponse>()
            .await
            .context("parsing the organization installation response")?;
        if installation.account.id != organization.id {
            bail!(
                "GitHub organization '{}' resolved to id {}, expected {}",
                organization.login,
                installation.account.id,
                organization.id
            );
        }

        let token = self.installation_token(installation.id).await?;
        let github_user_id_path = github_user_id.to_string();
        let user_url = self.api_url(["user", github_user_id_path.as_str()])?;
        let user = check_status(
            self.http
                .get(user_url)
                .header(reqwest::header::ACCEPT, GH_ACCEPT)
                .header("X-GitHub-Api-Version", GH_API_VERSION)
                .bearer_auth(&token)
                .send()
                .await
                .context("resolving the GitHub user by numeric id")?,
            "resolving the GitHub user by numeric id",
        )
        .await?
        .json::<GithubUserResponse>()
        .await
        .context("parsing the GitHub user response")?;
        if user.id != github_user_id {
            bail!(
                "GitHub user lookup returned id {}, expected {}",
                user.id,
                github_user_id
            );
        }
        let membership_url = self.api_url([
            "orgs",
            installation.account.login.as_str(),
            "memberships",
            user.login.as_str(),
        ])?;
        let response = self
            .http
            .get(membership_url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(token)
            .send()
            .await
            .context("checking GitHub organization membership")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let membership = check_status(response, "checking GitHub organization membership")
            .await?
            .json::<OrganizationMembershipResponse>()
            .await
            .context("parsing GitHub organization membership")?;
        if membership.organization.id != organization.id {
            bail!(
                "GitHub membership organization id {} did not match expected {}",
                membership.organization.id,
                organization.id
            );
        }
        if membership.user.id != github_user_id {
            bail!(
                "GitHub membership user id {} did not match expected {}",
                membership.user.id,
                github_user_id
            );
        }
        if !membership.user.login.eq_ignore_ascii_case(&user.login) {
            bail!(
                "GitHub membership login '{}' did not match resolved login '{}'",
                membership.user.login,
                user.login
            );
        }
        Ok(membership
            .state
            .eq_ignore_ascii_case("active")
            .then_some(user.login))
    }

    /// The cached token for `installation_id`, if one is present and still fresh.
    fn cached_token(&self, scope: &TokenScope) -> Option<String> {
        let now = Utc::now();
        let map = self.tokens.lock().expect("token cache mutex poisoned");
        map.get(scope)
            .filter(|t| t.is_fresh(now))
            .map(|t| t.token.clone())
    }

    /// Exchange `jwt` for an installation access token via
    /// `POST /app/installations/{id}/access_tokens`.
    async fn fetch_installation_token(
        &self,
        jwt: &str,
        installation_id: i64,
        repositories: &[String],
    ) -> Result<CachedToken> {
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.api_base, installation_id
        );
        let mut request = self
            .http
            .post(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(jwt);
        if !repositories.is_empty() {
            request = request.json(&serde_json::json!({
                "repositories": repositories,
                "permissions": {
                    "actions": "write",
                    "contents": "write",
                    "issues": "write",
                    "pull_requests": "write",
                    "workflows": "write"
                }
            }));
        }
        let resp = request
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
    pub async fn token_for_repo(&self, owner: &str, name: &str) -> Result<String> {
        let installation_id = self.installation_id(owner, name).await?;
        self.installation_token(installation_id).await
    }

    /// Mint a token constrained to the named repositories and the fixed write
    /// policy used by session profiles, including Actions and workflow files.
    pub async fn token_for_repositories(&self, repositories: &[String]) -> Result<String> {
        if repositories.is_empty() {
            bail!("GitHub repository allowlist is empty");
        }
        let mut slugs = repositories
            .iter()
            .map(|repository| crate::repo::parse_slug(repository).map_err(anyhow::Error::msg))
            .collect::<Result<Vec<_>>>()?;
        slugs.sort_by_key(crate::repo::RepoSlug::slug);
        slugs.dedup();
        let owner = slugs[0].owner.clone();
        if slugs.iter().any(|slug| slug.owner != owner) {
            bail!("GitHub repository allowlist must use one owner");
        }
        let scope =
            TokenScope::Repositories(slugs.iter().map(crate::repo::RepoSlug::slug).collect());
        if let Some(token) = self.cached_token(&scope) {
            return Ok(token);
        }
        let installation_id = self
            .installation_id(&slugs[0].owner, &slugs[0].name)
            .await?;
        for slug in &slugs[1..] {
            let candidate = self.installation_id(&slug.owner, &slug.name).await?;
            if candidate != installation_id {
                bail!("GitHub repositories do not share one App installation");
            }
        }
        let names = slugs.into_iter().map(|slug| slug.name).collect::<Vec<_>>();
        let jwt = self.current_jwt().await?;
        let fresh = self
            .fetch_installation_token(&jwt, installation_id, &names)
            .await?;
        let token = fresh.token.clone();
        self.tokens
            .lock()
            .expect("token cache mutex poisoned")
            .insert(scope, fresh);
        Ok(token)
    }

    async fn issue_json(&self, owner: &str, name: &str, number: i64) -> Result<serde_json::Value> {
        let token = self.token_for_repo(owner, name).await?;
        let url = format!("{}/repos/{owner}/{name}/issues/{number}", self.api_base);
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&token)
            .send()
            .await
            .context("fetching the issue")?;
        let resp = check_status(resp, "fetching the issue").await?;
        resp.json().await.context("parsing issue json")
    }

    /// Fetch the title, body, and URL used to seed a managed-repository launch.
    pub async fn issue(
        &self,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<crate::github::Issue> {
        let value = self.issue_json(owner, name, number).await?;
        Ok(crate::github::Issue {
            title: value["title"].as_str().unwrap_or_default().to_string(),
            body: value["body"].as_str().unwrap_or_default().to_string(),
            url: value["html_url"].as_str().unwrap_or_default().to_string(),
        })
    }

    // -- installation as allowlist ------------------------------------------

    /// When the App is installed on `slug`, add that repo to the managed
    /// allowlist if it isn't already there (idempotent), so the trigger's clone
    /// path accepts it. Callers reach this only *after* an
    /// [approved user][crate::github_trigger::authorize] has been authorized to
    /// trigger, so the person — not the repo owner — is the trust boundary: a
    /// stranger installing a *public* App on their own repo changes nothing,
    /// because no approved user will comment there. Best-effort and a no-op when
    /// the App is unconfigured, the repo is already registered, or the App is not
    /// installed on it (leaving the repos-table allowlist to govern).
    pub async fn ensure_installed_repo_registered(&self, slug: &RepoSlug) {
        if !self.is_configured().await {
            return;
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
    async fn post_issue_comment(&self, repo: &str, issue: i64, body: &str) -> Result<i64> {
        let slug = crate::repo::parse_slug(repo).map_err(|e| anyhow!(e))?;
        let id = self.thread_comment(&slug, issue, body).await?;
        tracing::info!(repo, issue, comment = id, "posted issue comment");
        Ok(id)
    }

    async fn update_issue_comment(&self, repo: &str, comment_id: i64, body: &str) -> Result<bool> {
        let slug = crate::repo::parse_slug(repo).map_err(|e| anyhow!(e))?;
        let token = self.token_for_repo(&slug.owner, &slug.name).await?;
        let url = format!(
            "{}/repos/{}/{}/issues/comments/{comment_id}",
            self.api_base, slug.owner, slug.name
        );
        let resp = self
            .http
            .patch(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .context("updating the issue comment")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        check_status(resp, "updating the issue comment").await?;
        tracing::info!(repo, comment = comment_id, "updated issue comment");
        Ok(true)
    }

    async fn react_to_comment(&self, repo: &str, comment_id: i64, content: &str) -> Result<()> {
        let slug = crate::repo::parse_slug(repo).map_err(|e| anyhow!(e))?;
        let token = self.token_for_repo(&slug.owner, &slug.name).await?;
        let url = format!(
            "{}/repos/{}/{}/issues/comments/{comment_id}/reactions",
            self.api_base, slug.owner, slug.name
        );
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await
            .context("reacting to the comment")?;
        check_status(resp, "reacting to the comment").await?;
        tracing::info!(repo, comment = comment_id, content, "reacted to comment");
        Ok(())
    }

    async fn issue_state(
        &self,
        repo: &str,
        number: i64,
    ) -> Result<crate::github_trigger::IssueState> {
        let slug = crate::repo::parse_slug(repo).map_err(|e| anyhow!(e))?;
        let v = self.issue_json(&slug.owner, &slug.name, number).await?;
        Ok(crate::github_trigger::IssueState {
            state: v["state"].as_str().unwrap_or_default().to_string(),
            title: v["title"].as_str().unwrap_or_default().to_string(),
            updated_at: v["updated_at"].as_str().unwrap_or_default().to_string(),
        })
    }

    async fn pr_head(&self, repo: &str, number: i64) -> Result<PrHead> {
        let slug = crate::repo::parse_slug(repo).map_err(|e| anyhow!(e))?;
        let token = self.token_for_repo(&slug.owner, &slug.name).await?;
        let url = format!(
            "{}/repos/{}/{}/pulls/{number}",
            self.api_base, slug.owner, slug.name
        );
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, GH_ACCEPT)
            .header("X-GitHub-Api-Version", GH_API_VERSION)
            .bearer_auth(&token)
            .send()
            .await
            .context("fetching the pull request")?;
        let resp = check_status(resp, "fetching the pull request").await?;
        let body: serde_json::Value = resp.json().await.context("parsing pull request json")?;
        let head_ref = body["head"]["ref"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("pull request #{number} has no head ref"))?
            .to_string();
        // A cross-repo PR's head lives in a fork (`head.repo.full_name` differs
        // from `base.repo.full_name`); GitHub also sets `head.repo` to null when
        // the fork was deleted, which we likewise treat as unpushable.
        let head_repo = body["head"]["repo"]["full_name"].as_str();
        let base_repo = body["base"]["repo"]["full_name"].as_str();
        let cross_repo = head_repo.is_none() || head_repo != base_repo;
        tracing::info!(
            repo,
            number,
            head_ref = %head_ref,
            cross_repo,
            "resolved pull request head"
        );
        Ok(PrHead {
            head_ref,
            cross_repo,
        })
    }
}

// ---------------------------------------------------------------------------
// REST response types + helpers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InstallationResponse {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct OrganizationInstallationResponse {
    id: i64,
    account: GithubOrganizationResponse,
}

#[derive(Debug, Deserialize)]
struct OrganizationMembershipResponse {
    state: String,
    organization: GithubOrganizationResponse,
    user: GithubUserResponse,
}

#[derive(Debug, Deserialize)]
struct GithubOrganizationResponse {
    id: i64,
    login: String,
}

#[derive(Debug, Deserialize)]
struct GithubUserResponse {
    id: i64,
    login: String,
}

#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    /// RFC 3339, e.g. `2016-07-11T22:14:10Z`.
    expires_at: String,
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
    tracing::warn!(what, %status, "github rest call failed");
    bail!("{what}: GitHub returned {status}: {}", body.trim())
}

#[cfg(any(test, feature = "test-support"))]
pub mod tests {
    // Under `test-support` this module is compiled for *another* crate's tests,
    // which reach for `configured_test_app` and `MOCK_INSTALLATION_TOKEN` and
    // leave the rest of the mock — and this crate's own unit tests — unbuilt.
    #![allow(dead_code, unused_imports)]

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Bytes;
    use axum::extract::{Json, Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, patch, post};
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
        installation_lookups: AtomicUsize,
        token_requests: Mutex<Vec<Value>>,
        /// Offset from now stamped as the token's `expires_at` (seconds). Negative
        /// → already expired, to exercise refresh.
        expiry_offset_secs: i64,
        comments: Mutex<Vec<Value>>,
        updates: Mutex<Vec<Value>>,
        reactions: Mutex<Vec<Value>>,
        last_comment_auth: Mutex<Option<String>>,
    }

    impl MockState {
        fn new(expiry_offset_secs: i64) -> Arc<Self> {
            Arc::new(Self {
                token_mints: AtomicUsize::new(0),
                installation_lookups: AtomicUsize::new(0),
                token_requests: Mutex::new(Vec::new()),
                expiry_offset_secs,
                comments: Mutex::new(Vec::new()),
                updates: Mutex::new(Vec::new()),
                reactions: Mutex::new(Vec::new()),
                last_comment_auth: Mutex::new(None),
            })
        }
    }

    /// The id the mock stamps on every created comment.
    const MOCK_COMMENT_ID: i64 = 4242;
    pub const MOCK_INSTALLATION_TOKEN: &str = "ghs_installation_token";
    /// A comment id the mock's PATCH route answers with 404, standing in for a
    /// comment a human deleted.
    const MOCK_DELETED_COMMENT_ID: i64 = 404_404;

    async fn mock_access_tokens(State(s): State<Arc<MockState>>, body: Bytes) -> Json<Value> {
        s.token_mints.fetch_add(1, Ordering::SeqCst);
        s.token_requests.lock().unwrap().push(if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        });
        let exp = Utc::now() + Duration::seconds(s.expiry_offset_secs);
        Json(json!({ "token": MOCK_INSTALLATION_TOKEN, "expires_at": exp.to_rfc3339() }))
    }

    async fn mock_installation(
        State(s): State<Arc<MockState>>,
        Path((owner, _name)): Path<(String, String)>,
    ) -> Result<Json<Value>, StatusCode> {
        s.installation_lookups.fetch_add(1, Ordering::SeqCst);
        // Simulate "the App is not installed on this repo" for a sentinel owner.
        if owner == "uninstalled" {
            return Err(StatusCode::NOT_FOUND);
        }
        Ok(Json(json!({ "id": 42 })))
    }

    async fn mock_organization_installation(
        State(s): State<Arc<MockState>>,
        Path(organization): Path<String>,
    ) -> Result<Json<Value>, StatusCode> {
        s.installation_lookups.fetch_add(1, Ordering::SeqCst);
        if organization == "broken" {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        let id = match organization.as_str() {
            "wrong-id" => 999,
            "Alternate" => 304,
            _ => 303,
        };
        Ok(Json(json!({
            "id": 42,
            "account": { "id": id, "login": organization }
        })))
    }

    async fn mock_user(Path(account_id): Path<i64>) -> Result<Json<Value>, StatusCode> {
        let (id, login) = match account_id {
            404 => (404, "member"),
            405 => (405, "former-member"),
            406 => (406, "invitee"),
            505 => (505, "renamed-member"),
            606 => (606, "departed-member"),
            707 => (707, "mismatched-member"),
            _ => return Err(StatusCode::NOT_FOUND),
        };
        Ok(Json(json!({ "id": id, "login": login })))
    }

    async fn mock_organization_membership(
        Path((organization, username)): Path<(String, String)>,
    ) -> Result<Json<Value>, StatusCode> {
        let state = match username.as_str() {
            "member" | "renamed-member" | "mismatched-member" => "active",
            "invitee" => "pending",
            _ => return Err(StatusCode::NOT_FOUND),
        };
        let organization_id = if organization == "Alternate" {
            304
        } else {
            303
        };
        let user_id = match username.as_str() {
            "member" => 404,
            "renamed-member" => 505,
            "mismatched-member" => 999,
            "invitee" => 406,
            _ => unreachable!(),
        };
        Ok(Json(json!({
            "state": state,
            "organization": { "id": organization_id, "login": organization },
            "user": { "id": user_id, "login": username }
        })))
    }

    async fn mock_comments(
        State(s): State<Arc<MockState>>,
        Path((owner, name, issue)): Path<(String, String, i64)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        *s.last_comment_auth.lock().unwrap() = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        s.comments.lock().unwrap().push(json!({
            "repo": format!("{owner}/{name}"),
            "issue": issue,
            "body": body["body"],
        }));
        (StatusCode::CREATED, Json(json!({ "id": MOCK_COMMENT_ID })))
    }

    async fn mock_update_comment(
        State(s): State<Arc<MockState>>,
        Path((owner, name, comment_id)): Path<(String, String, i64)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> StatusCode {
        if comment_id == MOCK_DELETED_COMMENT_ID {
            return StatusCode::NOT_FOUND;
        }
        *s.last_comment_auth.lock().unwrap() = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        s.updates.lock().unwrap().push(json!({
            "repo": format!("{owner}/{name}"),
            "comment": comment_id,
            "body": body["body"],
        }));
        StatusCode::OK
    }

    async fn mock_react(
        State(s): State<Arc<MockState>>,
        Path((owner, name, comment_id)): Path<(String, String, i64)>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        s.reactions.lock().unwrap().push(json!({
            "repo": format!("{owner}/{name}"),
            "comment": comment_id,
            "content": body["content"],
        }));
        (StatusCode::CREATED, Json(json!({ "id": 1 })))
    }

    async fn mock_issue(Path((owner, name, number)): Path<(String, String, i64)>) -> Json<Value> {
        Json(json!({
            "number": number,
            "state": "closed",
            "title": format!("issue {number} of {owner}/{name}"),
            "body": "issue body",
            "html_url": format!("https://github.com/{owner}/{name}/issues/{number}"),
            "updated_at": "2026-07-18T12:00:00Z",
        }))
    }

    async fn mock_edit_thread(
        Path((owner, name, number)): Path<(String, String, i64)>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        Json(json!({
            "number": number,
            "title": body["title"],
            "body": body["body"],
            "html_url": format!("https://github.com/{owner}/{name}/issues/{number}"),
            "state": "open",
        }))
    }

    async fn mock_labels() -> Json<Value> {
        Json(json!([]))
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
                "/orgs/{organization}/installation",
                get(mock_organization_installation),
            )
            .route(
                "/orgs/{organization}/memberships/{username}",
                get(mock_organization_membership),
            )
            .route("/user/{account_id}", get(mock_user))
            .route(
                "/repos/{owner}/{name}/issues/{issue}/comments",
                post(mock_comments),
            )
            .route(
                "/repos/{owner}/{name}/issues/{number}",
                get(mock_issue).patch(mock_edit_thread),
            )
            .route(
                "/repos/{owner}/{name}/pulls/{number}",
                get(mock_issue).patch(mock_edit_thread),
            )
            .route(
                "/repos/{owner}/{name}/issues/{number}/labels",
                post(mock_labels),
            )
            .route(
                "/repos/{owner}/{name}/issues/comments/{comment_id}",
                patch(mock_update_comment),
            )
            .route(
                "/repos/{owner}/{name}/issues/comments/{comment_id}/reactions",
                post(mock_react),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        weaver_core::spawn_boxed(Box::pin(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        format!("http://{addr}")
    }

    /// A configured `GithubApp` pointed at `api_base`, with the App id + test
    /// private key written to its (in-memory) settings — never via env, so unit
    /// tests stay parallel-safe.
    async fn configured_app(api_base: String) -> GithubApp {
        let db = crate::db::connect_in_memory().await.unwrap();
        configured_app_for_db(db, api_base).await
    }

    async fn configured_app_for_db(db: Db, api_base: String) -> GithubApp {
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
        GithubApp::with_parts(db, api_base)
    }

    pub async fn configured_test_app(db: Db) -> GithubApp {
        let base = spawn_mock(MockState::new(3600)).await;
        configured_app_for_db(db, base).await
    }

    // -- installation token exchange + caching ------------------------------

    /// A minted token is reused while fresh: a second request inside its lifetime
    /// hits the cache, not the GitHub token endpoint.
    #[tokio::test]
    async fn installation_token_is_cached_while_fresh() {
        let mock = MockState::new(3600); // expires an hour out
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;

        let t1 = app.installation_token(42).await.unwrap();
        let t2 = app.installation_token(42).await.unwrap();
        assert_eq!(t1, MOCK_INSTALLATION_TOKEN);
        assert_eq!(t1, t2);
        assert_eq!(
            mock.token_mints.load(Ordering::SeqCst),
            1,
            "the second call reused the cached token"
        );
    }

    #[tokio::test]
    async fn repository_token_is_downscoped_and_cached_by_allowlist() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;
        let repositories = vec![
            "marin-community/marin".to_string(),
            "marin-community/vllm".to_string(),
        ];

        app.token_for_repositories(&repositories).await.unwrap();
        app.token_for_repositories(&repositories).await.unwrap();

        assert_eq!(mock.token_mints.load(Ordering::SeqCst), 1);
        assert_eq!(mock.installation_lookups.load(Ordering::SeqCst), 2);
        assert_eq!(
            mock.token_requests.lock().unwrap().as_slice(),
            &[json!({
                "repositories": ["marin", "vllm"],
                "permissions": {
                    "actions": "write",
                    "contents": "write",
                    "issues": "write",
                    "pull_requests": "write",
                    "workflows": "write"
                }
            })]
        );
    }

    /// A token at/after its expiry is never served from cache: each request
    /// re-mints it.
    #[tokio::test]
    async fn installation_token_refreshes_once_expired() {
        let mock = MockState::new(-10); // already expired (inside the skew window)
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;

        app.installation_token(42).await.unwrap();
        app.installation_token(42).await.unwrap();
        assert_eq!(
            mock.token_mints.load(Ordering::SeqCst),
            2,
            "an expired token is re-minted, not reused"
        );
    }

    #[tokio::test]
    async fn organization_membership_uses_installation_and_immutable_id() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock).await;
        let app = configured_app(base).await;

        let organizations = vec![GithubOrganization {
            login: "Acme".to_string(),
            id: 303,
        }];
        assert_eq!(
            app.active_organization_membership(&organizations, 404)
                .await
                .unwrap()
                .unwrap()
                .github_login,
            "member"
        );
        assert!(app
            .active_organization_membership(&organizations, 406)
            .await
            .unwrap()
            .is_none());
        assert!(app
            .active_organization_membership(&organizations, 405)
            .await
            .unwrap()
            .is_none());

        let renamed = app
            .active_organization_membership(&organizations, 505)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.github_login, "renamed-member");
        assert!(app
            .active_organization_membership(&organizations, 606)
            .await
            .unwrap()
            .is_none());
        assert!(app
            .active_organization_membership(&organizations, 707)
            .await
            .unwrap_err()
            .to_string()
            .contains("expected 707"));

        let active_after_error = vec![
            GithubOrganization {
                login: "broken".to_string(),
                id: 302,
            },
            GithubOrganization {
                login: "Alternate".to_string(),
                id: 304,
            },
        ];
        assert!(app
            .active_organization_membership(&active_after_error, 404)
            .await
            .unwrap()
            .is_some());

        let wrong_id = vec![GithubOrganization {
            login: "wrong-id".to_string(),
            id: 303,
        }];
        assert!(app
            .active_organization_membership(&wrong_id, 404)
            .await
            .unwrap_err()
            .to_string()
            .contains("expected 303"));
    }

    // -- REST gateway calls -------------------------------------------------

    /// The reply posts over REST, authenticated with the installation token.
    #[tokio::test]
    async fn post_issue_comment_uses_installation_token() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;

        let id = app
            .post_issue_comment("acme/widgets", 7, "On it — http://loom/s/abc")
            .await
            .unwrap();
        assert_eq!(id, MOCK_COMMENT_ID, "the created comment's id comes back");

        let comments = mock.comments.lock().unwrap().clone();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["repo"], "acme/widgets");
        assert_eq!(comments[0]["issue"], 7);
        assert_eq!(comments[0]["body"], "On it — http://loom/s/abc");
        // The request carried the minted installation token, not the App JWT.
        assert_eq!(
            mock.last_comment_auth.lock().unwrap().clone(),
            Some(format!("Bearer {MOCK_INSTALLATION_TOKEN}")),
        );
    }

    #[tokio::test]
    async fn issue_fetch_maps_github_response() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock).await;
        let app = configured_app(base).await;

        let issue = app.issue("acme", "widgets", 7).await.unwrap();

        assert_eq!(issue.title, "issue 7 of acme/widgets");
        assert_eq!(issue.body, "issue body");
        assert_eq!(issue.url, "https://github.com/acme/widgets/issues/7");
    }

    #[tokio::test]
    async fn update_issue_comment_edits_in_place_and_flags_a_deleted_comment() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;

        let updated = app
            .update_issue_comment("acme/widgets", MOCK_COMMENT_ID, "On it — now with a trail")
            .await
            .unwrap();
        assert!(updated);
        let updates = mock.updates.lock().unwrap().clone();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["comment"], MOCK_COMMENT_ID);
        assert_eq!(updates[0]["body"], "On it — now with a trail");
        assert_eq!(
            mock.last_comment_auth.lock().unwrap().clone(),
            Some(format!("Bearer {MOCK_INSTALLATION_TOKEN}")),
        );

        // A 404 (the comment was deleted) is a clean `false`, not an error —
        // the caller reposts instead of retry-spamming.
        let updated = app
            .update_issue_comment("acme/widgets", MOCK_DELETED_COMMENT_ID, "orphaned edit")
            .await
            .unwrap();
        assert!(!updated);
    }

    // -- installation as the allowlist (§6.3) -------------------------------

    /// A repo the App is installed on is auto-registered into the managed
    /// allowlist so the trigger may clone it. Callers reach this only after an
    /// approved user is authorized, so the person is the trust boundary; there
    /// is no owner gate here.
    #[tokio::test]
    async fn installed_repo_is_auto_registered() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;

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

    /// A repo the App is **not installed on** is not auto-registered — the
    /// installation lookup 404s, so the clone path never accepts it. This is the
    /// only repo guard here: the person is authorized upstream, and the App
    /// install decides which repos loom can act on.
    #[tokio::test]
    async fn uninstalled_repo_is_not_auto_registered() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let app = configured_app(base).await;

        // The mock 404s the installation lookup for the "uninstalled" owner.
        let slug = crate::repo::parse_slug("uninstalled/evil").unwrap();
        app.ensure_installed_repo_registered(&slug).await;

        assert!(
            crate::repo::get_registered(&app.db, "uninstalled/evil")
                .await
                .unwrap()
                .is_none(),
            "a repo the App is not installed on must not be auto-registered"
        );
    }

    /// When the App is unconfigured the auto-register step is inert: the v1
    /// repos-table allowlist alone governs.
    #[tokio::test]
    async fn unconfigured_app_does_not_auto_register() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let db = crate::db::connect_in_memory().await.unwrap();
        let app = GithubApp::with_parts(db, base);

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

    // -- missing configuration ----------------------------------------------

    #[tokio::test]
    async fn unconfigured_app_fails_instead_of_using_an_ambient_credential() {
        let mock = MockState::new(3600);
        let base = spawn_mock(mock.clone()).await;
        let db = crate::db::connect_in_memory().await.unwrap();
        let app = GithubApp::with_parts(db, base);

        assert!(!app.is_configured().await);
        let error = app
            .post_issue_comment("acme/widgets", 7, "hi")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("GitHub App id is not configured"));
        assert_eq!(mock.token_mints.load(Ordering::SeqCst), 0);
        assert!(mock.comments.lock().unwrap().is_empty());
    }
}
