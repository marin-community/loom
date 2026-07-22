//! Short-lived automation credentials and GitHub Actions OIDC federation.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use jsonwebtoken::{
    decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use rand::RngCore;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use weaver_api::{AutomationTokenView, FederationReq, FederationView};

use crate::db::{now_iso, weaver_home, Db};

const ISSUER: &str = "loom";
const DEFAULT_AUDIENCE: &str = "loom";
const MAX_TTL_SECS: i64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FederationContext {
    pub repository_id: String,
    pub repository: String,
    pub workflow_ref: String,
    pub workflow_sha: String,
    pub event_name: String,
    pub git_ref: String,
    pub run_id: String,
    pub run_attempt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoomClaims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub grant: String,
    pub profiles: Vec<String>,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub jti: String,
    #[serde(default)]
    pub github: Option<FederationContext>,
}

fn key_path() -> PathBuf {
    weaver_home().join("loom-jwt.key")
}

fn signing_keys() -> Result<(EncodingKey, DecodingKey)> {
    let path = key_path();
    let private_key = match std::fs::read(&path) {
        Ok(value) if Ed25519KeyPair::from_pkcs8(&value).is_ok() => value,
        _ => {
            let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map_err(|_| anyhow!("generating Ed25519 automation signing key"))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, key.as_ref())
                .with_context(|| format!("writing {}", path.display()))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            key.as_ref().to_vec()
        }
    };
    let pair = Ed25519KeyPair::from_pkcs8(&private_key)
        .map_err(|_| anyhow!("loading Ed25519 automation signing key"))?;
    Ok((
        EncodingKey::from_ed_der(&private_key),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
    ))
}

async fn audience(db: &Db) -> String {
    let value = crate::config::get(db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if value.is_empty() {
        DEFAULT_AUDIENCE.to_string()
    } else {
        value
    }
}

pub async fn mint(
    db: &Db,
    subject: &str,
    profiles: Vec<String>,
    ttl_secs: i64,
    github: Option<FederationContext>,
) -> Result<AutomationTokenView> {
    let subject = subject.trim();
    if subject.is_empty() || profiles.is_empty() {
        bail!("automation subject and at least one profile are required");
    }
    if !(1..=MAX_TTL_SECS).contains(&ttl_secs) {
        bail!("automation token ttl must be between 1 and {MAX_TTL_SECS} seconds");
    }
    for profile_name in &profiles {
        let profile = crate::profile::get(db, profile_name)
            .await?
            .ok_or_else(|| anyhow!("unknown profile '{profile_name}'"))?;
        if !profile.is_automation_safe() {
            bail!("profile '{profile_name}' is not strict automation-safe");
        }
    }
    let now = chrono::Utc::now().timestamp();
    let expires_at = now + ttl_secs;
    let mut nonce = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce);
    let claims = LoomClaims {
        iss: ISSUER.to_string(),
        aud: audience(db).await,
        sub: subject.to_string(),
        grant: "automation".to_string(),
        profiles,
        iat: now,
        nbf: now - 5,
        exp: expires_at,
        jti: hex::encode(nonce),
        github,
    };
    let (encoding_key, _) = signing_keys()?;
    let token = encode(&Header::new(Algorithm::EdDSA), &claims, &encoding_key)?;
    Ok(AutomationTokenView { token, expires_at })
}

pub async fn verify(db: &Db, token: &str) -> Result<Option<LoomClaims>> {
    if token.matches('.').count() != 2 {
        return Ok(None);
    }
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[audience(db).await]);
    validation.validate_nbf = true;
    let (_, decoding_key) = signing_keys()?;
    let claims = match decode::<LoomClaims>(token, &decoding_key, &validation) {
        Ok(token) => token.claims,
        Err(_) => return Ok(None),
    };
    if claims.grant != "automation"
        || claims.profiles.is_empty()
        || claims.exp - claims.iat > MAX_TTL_SECS
    {
        return Ok(None);
    }
    Ok(Some(claims))
}

#[derive(Debug, Clone, FromRow)]
struct FederationRow {
    id: String,
    issuer: String,
    audience: String,
    repository_id: String,
    workflow_ref: String,
    event_name: Option<String>,
    ref_pattern: Option<String>,
    profile: String,
    created_at: String,
}

impl From<FederationRow> for FederationView {
    fn from(row: FederationRow) -> Self {
        Self {
            id: row.id,
            issuer: row.issuer,
            audience: row.audience,
            repository_id: row.repository_id,
            workflow_ref: row.workflow_ref,
            event_name: row.event_name,
            ref_pattern: row.ref_pattern,
            profile: row.profile,
            created_at: row.created_at,
        }
    }
}

pub async fn federation_add(db: &Db, req: &FederationReq) -> Result<FederationView> {
    let issuer = req.issuer.trim().trim_end_matches('/');
    let audience = req.audience.trim();
    if issuer.is_empty()
        || audience.is_empty()
        || req.repository_id.trim().is_empty()
        || req.workflow_ref.trim().is_empty()
    {
        bail!("issuer, audience, repository_id, and workflow_ref are required");
    }
    if audience.ends_with('/') {
        bail!("federation audience must not have a trailing slash");
    }
    let profile = crate::profile::get(db, req.profile.trim())
        .await?
        .ok_or_else(|| anyhow!("unknown profile '{}'", req.profile))?;
    if !profile.is_automation_safe() {
        bail!("federation profile must be automation-class, strict, and env-cleared");
    }
    let id = hex::encode(rand::random::<[u8; 8]>());
    sqlx::query(
        "INSERT INTO federation_mappings
         (id, issuer, audience, repository_id, workflow_ref, event_name, ref_pattern, profile, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(issuer)
    .bind(audience)
    .bind(req.repository_id.trim())
    .bind(req.workflow_ref.trim())
    .bind(req.event_name.as_deref().map(str::trim).filter(|v| !v.is_empty()))
    .bind(req.ref_pattern.as_deref().map(str::trim).filter(|v| !v.is_empty()))
    .bind(&profile.name)
    .bind(now_iso())
    .execute(db)
    .await?;
    federation_get(db, &id)
        .await?
        .ok_or_else(|| anyhow!("mapping vanished"))
}

pub async fn federation_list(db: &Db) -> Result<Vec<FederationView>> {
    Ok(sqlx::query_as::<_, FederationRow>(
        "SELECT * FROM federation_mappings ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

async fn federation_get(db: &Db, id: &str) -> Result<Option<FederationView>> {
    Ok(
        sqlx::query_as::<_, FederationRow>("SELECT * FROM federation_mappings WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?
            .map(Into::into),
    )
}

pub async fn federation_remove(db: &Db, id: &str) -> Result<bool> {
    Ok(sqlx::query("DELETE FROM federation_mappings WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Audience::One(value) => value == expected,
            Audience::Many(values) => values.iter().any(|value| value == expected),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubClaims {
    iss: String,
    aud: Audience,
    sub: String,
    repository_id: String,
    repository: String,
    workflow_ref: String,
    #[serde(default)]
    workflow_sha: String,
    event_name: String,
    #[serde(rename = "ref")]
    git_ref: String,
    run_id: String,
    run_attempt: String,
    #[serde(rename = "exp")]
    _exp: usize,
}

#[derive(Deserialize)]
struct Discovery {
    jwks_uri: String,
}

fn unverified_claims(token: &str) -> Result<GithubClaims> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow!("malformed OIDC token"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .context("decoding OIDC claims")?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn ref_matches(pattern: Option<&str>, value: &str) -> bool {
    match pattern {
        None => true,
        Some(pattern) if pattern.ends_with('*') => value.starts_with(&pattern[..pattern.len() - 1]),
        Some(pattern) => pattern == value,
    }
}

pub async fn federate(db: &Db, token: &str) -> Result<AutomationTokenView> {
    let hint = unverified_claims(token)?;
    let rows = sqlx::query_as::<_, FederationRow>(
        "SELECT * FROM federation_mappings
         WHERE issuer = ? AND repository_id = ? AND workflow_ref = ?",
    )
    .bind(hint.iss.trim_end_matches('/'))
    .bind(&hint.repository_id)
    .bind(&hint.workflow_ref)
    .fetch_all(db)
    .await?;
    let mapping = rows
        .into_iter()
        .find(|mapping| {
            hint.aud.contains(&mapping.audience)
                && mapping
                    .event_name
                    .as_deref()
                    .is_none_or(|event| event == hint.event_name)
                && ref_matches(mapping.ref_pattern.as_deref(), &hint.git_ref)
        })
        .ok_or_else(|| anyhow!("no federation mapping matches this workflow identity"))?;

    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        mapping.issuer.trim_end_matches('/')
    );
    let http = reqwest::Client::new();
    let discovery = http
        .get(discovery_url)
        .send()
        .await?
        .error_for_status()?
        .json::<Discovery>()
        .await?;
    let jwks = http
        .get(discovery.jwks_uri)
        .send()
        .await?
        .error_for_status()?
        .json::<jsonwebtoken::jwk::JwkSet>()
        .await?;
    let header = decode_header(token)?;
    let kid = header
        .kid
        .as_deref()
        .ok_or_else(|| anyhow!("OIDC token has no kid"))?;
    let jwk = jwks
        .find(kid)
        .ok_or_else(|| anyhow!("OIDC signing kid is unknown"))?;
    let key = DecodingKey::from_jwk(jwk)?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[mapping.issuer.as_str()]);
    validation.set_audience(&[mapping.audience.as_str()]);
    let verified = decode::<GithubClaims>(token, &key, &validation)?.claims;
    if verified.repository_id != mapping.repository_id
        || verified.workflow_ref != mapping.workflow_ref
        || mapping
            .event_name
            .as_deref()
            .is_some_and(|event| event != verified.event_name)
        || !ref_matches(mapping.ref_pattern.as_deref(), &verified.git_ref)
    {
        bail!("verified OIDC claims do not match the federation mapping");
    }
    let context = FederationContext {
        repository_id: verified.repository_id,
        repository: verified.repository,
        workflow_ref: verified.workflow_ref,
        workflow_sha: verified.workflow_sha,
        event_name: verified.event_name,
        git_ref: verified.git_ref,
        run_id: verified.run_id,
        run_attempt: verified.run_attempt,
    };
    // The exact verified workflow identity is the automation subject. Keep the
    // GitHub `sub` in it for audit while authorization remains keyed on stable
    // repository_id + workflow_ref.
    let subject = format!(
        "github:{}:{}:{}",
        mapping.repository_id, mapping.workflow_ref, verified.sub
    );
    mint(db, &subject, vec![mapping.profile], 600, Some(context)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn automation_profile(db: &Db) {
        crate::profile::upsert(
            db,
            &crate::profile::ProfileInput {
                name: "actions".to_string(),
                description: String::new(),
                agent_kind: "codex".to_string(),
                model: String::new(),
                effort: String::new(),
                protocol: "acp".to_string(),
                mode: "auto".to_string(),
                class: "automation".to_string(),
                strict: true,
                env_clear: true,
                ambient_allowlist: vec![],
                idle_archive_secs: Some(60),
                max_concurrent: 1,
                turn_budget: Some(10),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn loom_tokens_carry_only_automation_grants_and_are_bounded() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        std::env::set_var("LOOM_OWNER_GITHUB", "owner");
        let db = crate::db::connect_in_memory().await.unwrap();
        std::env::remove_var("LOOM_OWNER_GITHUB");
        automation_profile(&db).await;
        let minted = mint(
            &db,
            "github:repo:workflow",
            vec!["actions".to_string()],
            60,
            None,
        )
        .await
        .unwrap();
        let claims = verify(&db, &minted.token).await.unwrap().unwrap();
        assert_eq!(claims.grant, "automation");
        assert_eq!(claims.profiles, vec!["actions"]);
        assert!(mint(
            &db,
            "x",
            vec!["actions".to_string()],
            MAX_TTL_SECS + 1,
            None
        )
        .await
        .is_err());
        std::env::remove_var("WEAVER_HOME");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn github_oidc_signature_and_mapping_are_verified_before_context_is_copied() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        std::env::set_var("LOOM_OWNER_GITHUB", "owner");
        let db = crate::db::connect_in_memory().await.unwrap();
        std::env::remove_var("LOOM_OWNER_GITHUB");
        automation_profile(&db).await;

        let oidc_private = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let oidc_pair = Ed25519KeyPair::from_pkcs8(oidc_private.as_ref()).unwrap();
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(oidc_pair.public_key().as_ref());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let issuer = format!("http://{address}");
        let jwks_uri = format!("{issuer}/jwks");
        let discovery = serde_json::json!({ "jwks_uri": jwks_uri });
        let jwks = serde_json::json!({
            "keys": [{
                "kty": "OKP", "crv": "Ed25519", "x": x,
                "alg": "EdDSA", "use": "sig", "kid": "oidc-test"
            }]
        });
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get({
                    let discovery = discovery.clone();
                    move || async move { axum::Json(discovery) }
                }),
            )
            .route(
                "/jwks",
                axum::routing::get(move || async move { axum::Json(jwks) }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        federation_add(
            &db,
            &FederationReq {
                issuer: issuer.clone(),
                audience: "loom-test".to_string(),
                repository_id: "123".to_string(),
                workflow_ref: "acme/repo/.github/workflows/loom.yml@refs/heads/main".to_string(),
                event_name: Some("issues".to_string()),
                ref_pattern: Some("refs/heads/main".to_string()),
                profile: "actions".to_string(),
            },
        )
        .await
        .unwrap();
        let now = chrono::Utc::now().timestamp();
        let claims = GithubClaims {
            iss: issuer,
            aud: Audience::One("loom-test".to_string()),
            sub: "repo:acme/repo:ref:refs/heads/main".to_string(),
            repository_id: "123".to_string(),
            repository: "acme/repo".to_string(),
            workflow_ref: "acme/repo/.github/workflows/loom.yml@refs/heads/main".to_string(),
            workflow_sha: "abc123".to_string(),
            event_name: "issues".to_string(),
            git_ref: "refs/heads/main".to_string(),
            run_id: "99".to_string(),
            run_attempt: "2".to_string(),
            _exp: (now + 300) as usize,
        };
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("oidc-test".to_string());
        let oidc = encode(
            &header,
            &claims,
            &EncodingKey::from_ed_der(oidc_private.as_ref()),
        )
        .unwrap();
        let exchanged = federate(&db, &oidc).await.unwrap();
        let loom = verify(&db, &exchanged.token).await.unwrap().unwrap();
        let context = loom.github.unwrap();
        assert_eq!(context.repository, "acme/repo");
        assert_eq!(context.workflow_sha, "abc123");
        assert_eq!(context.run_attempt, "2");

        let mut tampered = oidc.into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'a' { b'b' } else { b'a' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(federate(&db, &tampered).await.is_err());
        server.abort();
        std::env::remove_var("WEAVER_HOME");
    }
}
