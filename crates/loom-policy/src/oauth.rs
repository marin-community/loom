//! Storage for Loom's OAuth 2.1 authorization server.
//!
//! The server exists so an outside MCP client — the claude.ai connector — can
//! obtain a person's Loom credential without anyone pasting a token. What it
//! issues is an ordinary API token: an `api_tokens` row of kind `oauth` that
//! carries the `user` grant whatever the person's role. `auth::lookup_token`
//! and every check behind it therefore apply unchanged, including the
//! organization authorization lease: a token whose user fails revalidation
//! stops working, exactly like a browser session.
//!
//! Around that token this module keeps the client registry, single-use
//! authorization codes, and rotating refresh tokens. Like `crate::auth`, it is
//! free of HTTP; the protocol endpoints live in `loom::web::oauth`.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use sqlx::Row;
use weaver_core::db::{iso_in_days, iso_in_seconds};

use crate::auth::{
    get_user, mint_token, random_b64, random_id, sha256_hex, Grant, SESSION_TTL_DAYS,
};
use crate::db::{now_iso, Db};

/// Access-token lifetime. It matches the one-hour organization lease, so a
/// refresh re-checks authorization about as often as revalidation runs.
pub const ACCESS_TOKEN_TTL_SECONDS: i64 = 60 * 60;
/// How long an authorization code may wait for its exchange.
pub const CODE_TTL_SECONDS: i64 = 5 * 60;
/// How long one consent lasts. Rotation keeps this deadline, so a client must
/// send the person back through consent after it, like a browser login.
pub const GRANT_TTL_DAYS: i64 = SESSION_TTL_DAYS;
/// Refresh tokens never start with the API-token prefix, so presenting one as
/// a bearer credential cannot match a row.
const REFRESH_TOKEN_PREFIX: &str = "loomrt_";
const CLIENT_SECRET_PREFIX: &str = "loomcs_";
const MAX_CLIENT_NAME_BYTES: usize = 100;

/// A registered OAuth client.
#[derive(Debug, Clone)]
pub struct Client {
    pub client_id: String,
    pub name: String,
    secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub created_by: String,
    pub created_at: String,
}

impl Client {
    /// Whether the client must authenticate with its secret at the token
    /// endpoint. A public client relies on PKCE alone.
    pub fn is_confidential(&self) -> bool {
        self.secret_hash.is_some()
    }

    /// Redirect URIs match exactly, as OAuth 2.1 requires, except that a
    /// loopback redirect matches on any port: a native client such as Claude
    /// Code listens on whatever port it is given (RFC 8252 §7.3).
    pub fn allows_redirect(&self, redirect_uri: &str) -> bool {
        self.redirect_uris.iter().any(|registered| {
            registered == redirect_uri || same_loopback_redirect(registered, redirect_uri)
        })
    }

    /// Whether `secret` authenticates this client. A public client presents
    /// none; a confidential one must present its own. Both sides are SHA-256
    /// digests of high-entropy values, so the comparison leaks nothing useful.
    pub fn authenticates(&self, secret: Option<&str>) -> bool {
        match (&self.secret_hash, secret) {
            (None, None) => true,
            (Some(expected), Some(secret)) => *expected == sha256_hex(secret),
            _ => false,
        }
    }
}

fn client_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Client> {
    let redirect_uris: String = row.get("redirect_uris");
    Ok(Client {
        client_id: row.get("client_id"),
        name: row.get("name"),
        secret_hash: row.get("secret_hash"),
        redirect_uris: serde_json::from_str(&redirect_uris)
            .context("invalid stored OAuth redirect URIs")?,
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
    })
}

fn is_loopback_http(url: &reqwest::Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
}

/// Whether `requested` is the registered loopback redirect on some port.
fn same_loopback_redirect(registered: &str, requested: &str) -> bool {
    let (Ok(mut registered), Ok(mut requested)) = (
        reqwest::Url::parse(registered),
        reqwest::Url::parse(requested),
    ) else {
        return false;
    };
    is_loopback_http(&registered)
        && registered.set_port(None).is_ok()
        && requested.set_port(None).is_ok()
        && registered == requested
}

/// A redirect URI an operator may register: an absolute HTTPS URL, or HTTP on
/// a loopback host for a native client (RFC 8252). Never a fragment.
pub fn validate_redirect_uri(uri: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(uri).map_err(|_| anyhow!("'{uri}' is not an absolute URL"))?;
    if parsed.fragment().is_some() {
        bail!("redirect URI '{uri}' must not contain a fragment");
    }
    if parsed.scheme() == "https" || is_loopback_http(&parsed) {
        return Ok(());
    }
    bail!("redirect URI '{uri}' must use https, or http on a loopback host")
}

/// Register a client. Returns the client and, for a confidential one, its
/// secret — the only time the plaintext exists.
pub async fn create_client(
    db: &Db,
    name: &str,
    redirect_uris: &[String],
    confidential: bool,
    created_by: &str,
) -> Result<(Client, Option<String>)> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a client name is required");
    }
    if name.len() > MAX_CLIENT_NAME_BYTES {
        bail!("a client name must be at most {MAX_CLIENT_NAME_BYTES} bytes");
    }
    if redirect_uris.is_empty() {
        bail!("at least one redirect URI is required");
    }
    for uri in redirect_uris {
        validate_redirect_uri(uri)?;
    }
    let secret = confidential.then(|| format!("{CLIENT_SECRET_PREFIX}{}", random_b64(32)));
    let client_id = format!("loom-{}", random_id());
    sqlx::query(
        "INSERT INTO oauth_clients
           (client_id, name, secret_hash, redirect_uris, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&client_id)
    .bind(name)
    .bind(secret.as_deref().map(sha256_hex))
    .bind(serde_json::to_string(redirect_uris)?)
    .bind(created_by)
    .bind(now_iso())
    .execute(db)
    .await
    .context("registering OAuth client")?;
    let client = get_client(db, &client_id)
        .await?
        .ok_or_else(|| anyhow!("OAuth client vanished after insert"))?;
    Ok((client, secret))
}

pub async fn get_client(db: &Db, client_id: &str) -> Result<Option<Client>> {
    sqlx::query("SELECT * FROM oauth_clients WHERE client_id = ?")
        .bind(client_id)
        .fetch_optional(db)
        .await?
        .as_ref()
        .map(client_from_row)
        .transpose()
}

pub async fn list_clients(db: &Db) -> Result<Vec<Client>> {
    sqlx::query("SELECT * FROM oauth_clients ORDER BY created_at, client_id")
        .fetch_all(db)
        .await?
        .iter()
        .map(client_from_row)
        .collect()
}

/// Remove a client and revoke everything issued to it. Returns `None` for an
/// unknown client, otherwise how many access and refresh credentials went
/// with it. Codes and refresh tokens cascade with the client row.
pub async fn remove_client(db: &Db, client_id: &str) -> Result<Option<i64>> {
    let mut tx = db.begin().await?;
    let access = sqlx::query(
        "DELETE FROM api_tokens WHERE kind = 'oauth' AND id IN (
           SELECT access_token_id FROM oauth_refresh_tokens WHERE client_id = ?
         )",
    )
    .bind(client_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let refresh: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM oauth_refresh_tokens WHERE client_id = ?")
            .bind(client_id)
            .fetch_one(&mut *tx)
            .await?;
    let removed = sqlx::query("DELETE FROM oauth_clients WHERE client_id = ?")
        .bind(client_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok((removed > 0).then_some(access as i64 + refresh))
}

/// What a person approved, held under a single-use code until the client
/// exchanges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode {
    pub client_id: String,
    pub username: String,
    pub redirect_uri: String,
    pub code_challenge: String,
}

/// Record an approval and return the code to send back to the client.
pub async fn create_code(db: &Db, approval: &AuthorizationCode) -> Result<String> {
    let now = now_iso();
    // Unused codes are worthless once expired; drop them as new ones arrive.
    sqlx::query("DELETE FROM oauth_codes WHERE expires_at <= ?")
        .bind(&now)
        .execute(db)
        .await?;
    let code = random_b64(32);
    let expires_at = iso_in_seconds(CODE_TTL_SECONDS)
        .ok_or_else(|| anyhow!("authorization code expiry overflowed"))?;
    sqlx::query(
        "INSERT INTO oauth_codes
           (code_hash, client_id, username, redirect_uri, code_challenge, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(sha256_hex(&code))
    .bind(&approval.client_id)
    .bind(&approval.username)
    .bind(&approval.redirect_uri)
    .bind(&approval.code_challenge)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(code)
}

/// Consume a code. It is deleted whether or not it is still valid, so a code
/// can be presented once; `None` means unknown, already used, or expired.
pub async fn redeem_code(db: &Db, code: &str) -> Result<Option<AuthorizationCode>> {
    let row = sqlx::query(
        "DELETE FROM oauth_codes WHERE code_hash = ?
         RETURNING client_id, username, redirect_uri, code_challenge, expires_at",
    )
    .bind(sha256_hex(code))
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let expires_at: String = row.get("expires_at");
    if expires_at <= now_iso() {
        return Ok(None);
    }
    Ok(Some(AuthorizationCode {
        client_id: row.get("client_id"),
        username: row.get("username"),
        redirect_uri: row.get("redirect_uri"),
        code_challenge: row.get("code_challenge"),
    }))
}

/// PKCE S256: the verifier hashes to the challenge the client sent with its
/// authorization request (RFC 7636 §4.6).
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let well_formed = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    if !well_formed {
        return false;
    }
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest) == challenge
}

/// The credentials one token response carries.
#[derive(Debug, Clone)]
pub struct IssuedTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

/// Whether `username` holds a manual approval or a current organization lease
/// right now.
async fn authorized_now(db: &Db, username: &str) -> Result<bool> {
    Ok(get_user(db, username)
        .await?
        .is_some_and(|user| user.is_authorized_at(&now_iso())))
}

/// Issue the first access and refresh tokens for an approval. Fails when the
/// person's authorization lapsed between consent and exchange.
pub async fn issue_tokens(db: &Db, client: &Client, username: &str) -> Result<IssuedTokens> {
    if !authorized_now(db, username).await? {
        bail!("authorization for user '{username}' has expired");
    }
    let grant_expires_at =
        iso_in_days(GRANT_TTL_DAYS).ok_or_else(|| anyhow!("OAuth grant expiry overflowed"))?;
    let mut tx = db.begin().await?;
    let issued = insert_tokens(&mut tx, client, username, &grant_expires_at).await?;
    tx.commit().await?;
    Ok(issued)
}

/// Exchange a refresh token for a new pair, revoking the old pair.
///
/// The claim is one statement: the token must belong to `client`, its grant
/// must be unexpired, and its user must be authorized *now*. Refresh re-checks
/// the organization lease rather than extending it; while the lease is lapsed
/// the token is refused but kept, so it works again if revalidation succeeds,
/// as a browser cookie would.
pub async fn refresh_tokens(
    db: &Db,
    client: &Client,
    refresh_token: &str,
) -> Result<Option<IssuedTokens>> {
    let now = now_iso();
    let mut tx = db.begin().await?;
    let claimed = sqlx::query(
        "DELETE FROM oauth_refresh_tokens
         WHERE token_hash = ? AND client_id = ? AND expires_at > ?
           AND username IN (
             SELECT username FROM users
             WHERE authorization_kind = 'manual' OR authorization_valid_until > ?
           )
         RETURNING username, access_token_id, expires_at",
    )
    .bind(sha256_hex(refresh_token))
    .bind(&client.client_id)
    .bind(&now)
    .bind(&now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(claimed) = claimed else {
        return Ok(None);
    };
    let username: String = claimed.get("username");
    let previous_access: String = claimed.get("access_token_id");
    let grant_expires_at: String = claimed.get("expires_at");
    sqlx::query("DELETE FROM api_tokens WHERE id = ? AND kind = 'oauth'")
        .bind(&previous_access)
        .execute(&mut *tx)
        .await?;
    let issued = insert_tokens(&mut tx, client, &username, &grant_expires_at).await?;
    tx.commit().await?;
    Ok(Some(issued))
}

async fn insert_tokens(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    client: &Client,
    username: &str,
    grant_expires_at: &str,
) -> Result<IssuedTokens> {
    let (access_token, access_hash, prefix) = mint_token();
    let access_id = random_id();
    let access_expires_at = iso_in_seconds(ACCESS_TOKEN_TTL_SECONDS)
        .ok_or_else(|| anyhow!("access token expiry overflowed"))?;
    sqlx::query(
        "INSERT INTO api_tokens
           (id, username, name, token_hash, prefix, kind, expires_at, grant_json)
         VALUES (?, ?, ?, ?, ?, 'oauth', ?, ?)",
    )
    .bind(&access_id)
    .bind(username)
    .bind(format!("OAuth: {}", client.name))
    .bind(&access_hash)
    .bind(&prefix)
    .bind(&access_expires_at)
    // An admin's connector works with user authority; administration stays in
    // the browser and the CLI.
    .bind(serde_json::to_string(&Grant::User)?)
    .execute(&mut **tx)
    .await
    .context("issuing OAuth access token")?;
    let refresh_token = format!("{REFRESH_TOKEN_PREFIX}{}", random_b64(32));
    sqlx::query(
        "INSERT INTO oauth_refresh_tokens
           (token_hash, client_id, username, access_token_id, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(sha256_hex(&refresh_token))
    .bind(&client.client_id)
    .bind(username)
    .bind(&access_id)
    .bind(now_iso())
    .bind(grant_expires_at)
    .execute(&mut **tx)
    .await
    .context("issuing OAuth refresh token")?;
    Ok(IssuedTokens {
        access_token,
        refresh_token,
        expires_in: ACCESS_TOKEN_TTL_SECONDS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{add_user, lookup_token, Grant, UserRole};
    use crate::db;

    const CLAUDE_CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    async fn setup() -> (Db, Client) {
        let db = db::connect_in_memory().await.unwrap();
        add_user(&db, "ada", None, None, None, UserRole::Admin)
            .await
            .unwrap();
        let (client, secret) = create_client(
            &db,
            "claude.ai",
            &[CLAUDE_CALLBACK.to_string()],
            true,
            "ada",
        )
        .await
        .unwrap();
        assert!(client.authenticates(secret.as_deref()));
        (db, client)
    }

    fn approval(client: &Client) -> AuthorizationCode {
        AuthorizationCode {
            client_id: client.client_id.clone(),
            username: "ada".to_string(),
            redirect_uri: CLAUDE_CALLBACK.to_string(),
            code_challenge: CHALLENGE.to_string(),
        }
    }

    #[test]
    fn pkce_accepts_only_the_rfc_example_verifier() {
        assert!(pkce_matches(VERIFIER, CHALLENGE));
        assert!(!pkce_matches(&VERIFIER.replace('d', "e"), CHALLENGE));
        assert!(!pkce_matches("short", CHALLENGE));
    }

    #[test]
    fn redirect_uris_must_be_https_or_loopback_http() {
        for uri in [
            CLAUDE_CALLBACK,
            "http://127.0.0.1:33418/callback",
            "http://localhost:6274/oauth/callback",
        ] {
            validate_redirect_uri(uri).unwrap();
        }
        for uri in [
            "http://claude.ai/callback",
            "https://claude.ai/callback#frag",
            "claude.ai/callback",
            "javascript:alert(1)",
        ] {
            assert!(validate_redirect_uri(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn loopback_redirects_match_on_any_port_and_others_match_exactly() {
        let client = Client {
            client_id: "loom-test".to_string(),
            name: "test".to_string(),
            secret_hash: None,
            redirect_uris: vec![
                CLAUDE_CALLBACK.to_string(),
                "http://localhost/callback".to_string(),
                "http://127.0.0.1:8080/callback".to_string(),
            ],
            created_by: "ada".to_string(),
            created_at: String::new(),
        };
        for allowed in [
            CLAUDE_CALLBACK,
            "http://localhost/callback",
            "http://localhost:3118/callback",
            "http://127.0.0.1/callback",
            "http://127.0.0.1:49152/callback",
        ] {
            assert!(client.allows_redirect(allowed), "{allowed}");
        }
        for refused in [
            "https://claude.ai/api/mcp/auth_callback/",
            "https://claude.ai:8443/api/mcp/auth_callback",
            "http://localhost:3118/other",
            "http://localhost:3118/callback?next=x",
            "https://localhost:3118/callback",
            "http://127.0.0.2:3118/callback",
            "http://localhost.example.com/callback",
        ] {
            assert!(!client.allows_redirect(refused), "{refused}");
        }
    }

    #[tokio::test]
    async fn a_code_is_single_use_and_yields_a_user_grant_token() {
        let (db, client) = setup().await;
        let code = create_code(&db, &approval(&client)).await.unwrap();
        assert_eq!(
            redeem_code(&db, &code).await.unwrap(),
            Some(approval(&client))
        );
        assert_eq!(redeem_code(&db, &code).await.unwrap(), None);

        let issued = issue_tokens(&db, &client, "ada").await.unwrap();
        let principal = lookup_token(&db, &issued.access_token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.username, "ada");
        // The person is an admin, but the connector holds user authority.
        assert_eq!(principal.grant, Grant::User);
        assert!(lookup_token(&db, &issued.refresh_token)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn refresh_rotates_both_tokens_and_rejects_the_old_refresh_token() {
        let (db, client) = setup().await;
        let first = issue_tokens(&db, &client, "ada").await.unwrap();
        let second = refresh_tokens(&db, &client, &first.refresh_token)
            .await
            .unwrap()
            .unwrap();
        assert!(lookup_token(&db, &first.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(lookup_token(&db, &second.access_token)
            .await
            .unwrap()
            .is_some());
        assert!(refresh_tokens(&db, &client, &first.refresh_token)
            .await
            .unwrap()
            .is_none());

        let (other, _) = create_client(&db, "other", &[CLAUDE_CALLBACK.to_string()], false, "ada")
            .await
            .unwrap();
        assert!(
            refresh_tokens(&db, &other, &second.refresh_token)
                .await
                .unwrap()
                .is_none(),
            "a refresh token is bound to the client it was issued to"
        );
    }

    #[tokio::test]
    async fn a_lapsed_organization_lease_stops_access_and_refresh() {
        let (db, client) = setup().await;
        add_user(&db, "grace", Some("grace"), Some(7), None, UserRole::User)
            .await
            .unwrap();
        let set_lease = |valid_until: String| {
            sqlx::query(
                "UPDATE users SET authorization_kind = 'github_organization',
                    authorization_github_org_id = 1, authorization_github_org_login = 'org',
                    authorization_valid_until = ?
                 WHERE username = 'grace'",
            )
            .bind(valid_until)
            .execute(&db)
        };
        set_lease(iso_in_seconds(3600).unwrap()).await.unwrap();
        let issued = issue_tokens(&db, &client, "grace").await.unwrap();
        assert!(lookup_token(&db, &issued.access_token)
            .await
            .unwrap()
            .is_some());

        set_lease(iso_in_seconds(-1).unwrap()).await.unwrap();
        assert!(lookup_token(&db, &issued.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(refresh_tokens(&db, &client, &issued.refresh_token)
            .await
            .unwrap()
            .is_none());
        assert!(issue_tokens(&db, &client, "grace").await.is_err());

        // Revalidation renews the lease; the kept refresh token works again.
        set_lease(iso_in_seconds(3600).unwrap()).await.unwrap();
        assert!(refresh_tokens(&db, &client, &issued.refresh_token)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn removing_a_client_revokes_what_it_was_issued() {
        let (db, client) = setup().await;
        let issued = issue_tokens(&db, &client, "ada").await.unwrap();
        create_code(&db, &approval(&client)).await.unwrap();
        assert_eq!(
            remove_client(&db, &client.client_id).await.unwrap(),
            Some(2)
        );
        assert!(lookup_token(&db, &issued.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(get_client(&db, &client.client_id).await.unwrap().is_none());
        assert_eq!(remove_client(&db, &client.client_id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clients_authenticate_by_kind() {
        let (db, confidential) = setup().await;
        assert!(!confidential.authenticates(None));
        assert!(!confidential.authenticates(Some("loomcs_wrong")));
        let (public, secret) = create_client(
            &db,
            "inspector",
            &[CLAUDE_CALLBACK.to_string()],
            false,
            "ada",
        )
        .await
        .unwrap();
        assert!(secret.is_none());
        assert!(public.authenticates(None));
        assert!(!public.authenticates(Some("anything")));
        assert_eq!(list_clients(&db).await.unwrap().len(), 2);
    }
}
