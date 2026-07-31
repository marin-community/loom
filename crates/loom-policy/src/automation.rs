//! Short-lived automation credentials and provider-backed OIDC federation.

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
pub struct GithubContext {
    pub repository_id: String,
    pub repository: String,
    pub workflow_ref: String,
    pub workflow_sha: String,
    pub event_name: String,
    pub git_ref: String,
    pub run_id: String,
    pub run_attempt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FederationContext {
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub service_tag: String,
    #[serde(default)]
    pub service_account: Option<String>,
    #[serde(default)]
    pub github: Option<GithubContext>,
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
    pub provider: Option<String>,
    #[serde(default)]
    pub service_tag: Option<String>,
    #[serde(default)]
    pub federation: Option<FederationContext>,
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
    federation: Option<FederationContext>,
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
        provider: federation.as_ref().map(|context| context.provider.clone()),
        service_tag: federation
            .as_ref()
            .map(|context| context.service_tag.clone()),
        federation,
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
    name: String,
    provider: String,
    issuer: String,
    audience: String,
    subject: Option<String>,
    service_account: Option<String>,
    service_tag: String,
    repository_id: Option<String>,
    workflow_ref: Option<String>,
    event_name: Option<String>,
    ref_pattern: Option<String>,
    profiles_json: String,
    created_at: String,
    updated_at: String,
}

impl FederationRow {
    fn profiles(&self) -> Result<Vec<String>> {
        serde_json::from_str(&self.profiles_json).context("invalid federation profiles")
    }

    fn view(&self) -> Result<FederationView> {
        Ok(FederationView {
            id: self.id.clone(),
            name: self.name.clone(),
            provider: self.provider.clone(),
            issuer: self.issuer.clone(),
            audience: self.audience.clone(),
            subject: self.subject.clone(),
            service_account: self.service_account.clone(),
            service_tag: self.service_tag.clone(),
            repository_id: self.repository_id.clone(),
            workflow_ref: self.workflow_ref.clone(),
            event_name: self.event_name.clone(),
            ref_pattern: self.ref_pattern.clone(),
            profiles: self.profiles()?,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        })
    }
}

pub async fn federation_add(db: &Db, req: &FederationReq) -> Result<FederationView> {
    let name = req.name.trim();
    crate::profile::validate_name(name).map_err(|error| anyhow!(error))?;
    let provider = req.provider.trim().to_ascii_lowercase();
    if !matches!(provider.as_str(), "github" | "google") {
        bail!("federation provider must be 'github' or 'google'");
    }
    let issuer = req.issuer.trim();
    let audience = req.audience.trim();
    if issuer.is_empty() || audience.is_empty() {
        bail!("issuer and audience are required");
    }
    if issuer.ends_with('/') || audience.ends_with('/') {
        bail!("federation issuer and audience must not have a trailing slash");
    }
    let service_tag = req.service_tag.trim();
    if service_tag.is_empty()
        || service_tag.len() > 64
        || !service_tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("service_tag must be 1-64 portable label characters");
    }
    let mut profiles: Vec<String> = req
        .profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .collect();
    profiles.sort();
    profiles.dedup();
    if profiles.is_empty() || profiles.iter().any(String::is_empty) {
        bail!("at least one federation profile is required");
    }
    for profile_name in &profiles {
        let profile = crate::profile::get(db, profile_name)
            .await?
            .ok_or_else(|| anyhow!("unknown profile '{profile_name}'"))?;
        if !profile.is_automation_safe() {
            bail!("federation profiles must be automation-class, strict, and env-cleared");
        }
    }
    let optional = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let subject = optional(req.subject.as_deref());
    let service_account = optional(req.service_account.as_deref());
    let repository_id = optional(req.repository_id.as_deref());
    let workflow_ref = optional(req.workflow_ref.as_deref());
    match provider.as_str() {
        "github" => {
            if repository_id.is_none() || workflow_ref.is_none() {
                bail!("GitHub federation requires repository_id and workflow_ref");
            }
            if subject.is_some() || service_account.is_some() {
                bail!("GitHub federation does not accept Google identity fields");
            }
        }
        "google" => {
            if issuer != "https://accounts.google.com" {
                bail!("Google federation issuer must be https://accounts.google.com");
            }
            if !subject
                .as_deref()
                .is_some_and(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
                || !service_account
                    .as_deref()
                    .is_some_and(|value| value.ends_with(".iam.gserviceaccount.com"))
            {
                bail!("Google federation requires a numeric subject and service-account email");
            }
            if repository_id.is_some() || workflow_ref.is_some() {
                bail!("Google federation does not accept GitHub identity fields");
            }
        }
        _ => unreachable!(),
    }
    let id = hex::encode(rand::random::<[u8; 8]>());
    let now = now_iso();
    let profiles_json = serde_json::to_string(&profiles)?;
    sqlx::query(
        "INSERT INTO federation_mappings
         (id, name, provider, issuer, audience, subject, service_account,
          service_tag, repository_id, workflow_ref, event_name, ref_pattern,
          profiles_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET
          provider=excluded.provider, issuer=excluded.issuer,
          audience=excluded.audience, subject=excluded.subject,
          service_account=excluded.service_account,
          service_tag=excluded.service_tag,
          repository_id=excluded.repository_id,
          workflow_ref=excluded.workflow_ref, event_name=excluded.event_name,
          ref_pattern=excluded.ref_pattern, profiles_json=excluded.profiles_json,
          updated_at=excluded.updated_at
         WHERE provider IS NOT excluded.provider OR issuer IS NOT excluded.issuer
            OR audience IS NOT excluded.audience OR subject IS NOT excluded.subject
            OR service_account IS NOT excluded.service_account
            OR service_tag IS NOT excluded.service_tag
            OR repository_id IS NOT excluded.repository_id
            OR workflow_ref IS NOT excluded.workflow_ref
            OR event_name IS NOT excluded.event_name
            OR ref_pattern IS NOT excluded.ref_pattern
            OR profiles_json IS NOT excluded.profiles_json",
    )
    .bind(&id)
    .bind(name)
    .bind(&provider)
    .bind(issuer)
    .bind(audience)
    .bind(subject)
    .bind(service_account)
    .bind(service_tag)
    .bind(repository_id)
    .bind(workflow_ref)
    .bind(
        req.event_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(
        req.ref_pattern
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(profiles_json)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    federation_get(db, name)
        .await?
        .ok_or_else(|| anyhow!("mapping vanished"))
}

pub async fn federation_list(db: &Db) -> Result<Vec<FederationView>> {
    sqlx::query_as::<_, FederationRow>("SELECT * FROM federation_mappings ORDER BY created_at DESC")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|row| row.view())
        .collect()
}

pub async fn federation_get(db: &Db, key: &str) -> Result<Option<FederationView>> {
    sqlx::query_as::<_, FederationRow>("SELECT * FROM federation_mappings WHERE id = ? OR name = ?")
        .bind(key)
        .bind(key)
        .fetch_optional(db)
        .await?
        .map(|row| row.view())
        .transpose()
}

pub async fn federation_remove(db: &Db, key: &str) -> Result<bool> {
    Ok(
        sqlx::query("DELETE FROM federation_mappings WHERE id = ? OR name = ?")
            .bind(key)
            .bind(key)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

pub async fn federation_mark_deployment_managed(db: &Db, name: &str) -> Result<()> {
    sqlx::query("UPDATE federation_mappings SET managed_by_deployment = 1 WHERE name = ?")
        .bind(name)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn deployment_managed_federation_names(db: &Db) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM federation_mappings
         WHERE managed_by_deployment = 1 ORDER BY name",
    )
    .fetch_all(db)
    .await?)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GoogleClaims {
    iss: String,
    aud: Audience,
    sub: String,
    email: String,
    email_verified: bool,
    #[serde(rename = "exp")]
    _exp: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct OidcHint {
    iss: String,
    aud: Audience,
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    repository_id: Option<String>,
    #[serde(default)]
    workflow_ref: Option<String>,
    #[serde(default)]
    event_name: Option<String>,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
}

#[derive(Deserialize)]
struct Discovery {
    jwks_uri: String,
}

fn unverified_claims(token: &str) -> Result<OidcHint> {
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
    let rows =
        sqlx::query_as::<_, FederationRow>("SELECT * FROM federation_mappings WHERE issuer = ?")
            .bind(&hint.iss)
            .fetch_all(db)
            .await?;
    let mut matches = rows
        .into_iter()
        .filter(|mapping| {
            if !hint.aud.contains(&mapping.audience) {
                return false;
            }
            match mapping.provider.as_str() {
                "github" => {
                    mapping.repository_id.as_deref() == hint.repository_id.as_deref()
                        && mapping.workflow_ref.as_deref() == hint.workflow_ref.as_deref()
                        && mapping
                            .event_name
                            .as_deref()
                            .is_none_or(|event| Some(event) == hint.event_name.as_deref())
                        && hint.git_ref.as_deref().is_some_and(|git_ref| {
                            ref_matches(mapping.ref_pattern.as_deref(), git_ref)
                        })
                }
                "google" => {
                    mapping.subject.as_deref() == Some(hint.sub.as_str())
                        && mapping.service_account.as_deref() == hint.email.as_deref()
                }
                _ => false,
            }
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        bail!("multiple federation mappings match this workload identity");
    }
    let mapping = matches
        .pop()
        .ok_or_else(|| anyhow!("no federation mapping matches this workload identity"))?;

    let discovery_url = format!("{}/.well-known/openid-configuration", mapping.issuer);
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
    if !matches!(header.alg, Algorithm::RS256 | Algorithm::EdDSA)
        || ((mapping.provider == "google"
            || mapping.issuer == "https://token.actions.githubusercontent.com")
            && header.alg != Algorithm::RS256)
    {
        bail!("OIDC signing algorithm is not allowed for this provider");
    }
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
    let profiles = mapping.profiles()?;
    let (subject, context) = match mapping.provider.as_str() {
        "github" => {
            let verified = decode::<GithubClaims>(token, &key, &validation)?.claims;
            if mapping.repository_id.as_deref() != Some(verified.repository_id.as_str())
                || mapping.workflow_ref.as_deref() != Some(verified.workflow_ref.as_str())
                || mapping
                    .event_name
                    .as_deref()
                    .is_some_and(|event| event != verified.event_name)
                || !ref_matches(mapping.ref_pattern.as_deref(), &verified.git_ref)
            {
                bail!("verified OIDC claims do not match the federation mapping");
            }
            let subject = format!(
                "github:{}:{}:{}",
                verified.repository_id, verified.workflow_ref, verified.sub
            );
            let github = GithubContext {
                repository_id: verified.repository_id,
                repository: verified.repository,
                workflow_ref: verified.workflow_ref,
                workflow_sha: verified.workflow_sha,
                event_name: verified.event_name,
                git_ref: verified.git_ref,
                run_id: verified.run_id,
                run_attempt: verified.run_attempt,
            };
            let context = FederationContext {
                provider: mapping.provider.clone(),
                issuer: mapping.issuer.clone(),
                subject: subject.clone(),
                service_tag: mapping.service_tag.clone(),
                service_account: None,
                github: Some(github),
            };
            (subject, context)
        }
        "google" => {
            let verified = decode::<GoogleClaims>(token, &key, &validation)?.claims;
            if !verified.email_verified
                || mapping.subject.as_deref() != Some(verified.sub.as_str())
                || mapping.service_account.as_deref() != Some(verified.email.as_str())
            {
                bail!("verified Google identity does not match the federation mapping");
            }
            let subject = format!("google:{}:{}", verified.sub, verified.email);
            let context = FederationContext {
                provider: mapping.provider.clone(),
                issuer: verified.iss,
                subject: subject.clone(),
                service_tag: mapping.service_tag.clone(),
                service_account: Some(verified.email),
                github: None,
            };
            (subject, context)
        }
        _ => bail!("unsupported federation provider"),
    };
    mint(db, &subject, profiles, 600, Some(context)).await
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
                prelude: "weaver".to_string(),
                restricted: false,
                allowed_tools: vec![],
                mcp_access: weaver_api::McpAccess::default(),
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
    async fn google_workload_mappings_are_exact_and_idempotent() {
        let db = crate::db::connect_in_memory().await.unwrap();
        automation_profile(&db).await;
        let req = FederationReq {
            name: "marin-ops".to_string(),
            provider: "google".to_string(),
            issuer: "https://accounts.google.com".to_string(),
            audience: "https://loom.example.com".to_string(),
            subject: Some("11223344556677889900".to_string()),
            service_account: Some("loom-marin-ops@acme.iam.gserviceaccount.com".to_string()),
            service_tag: "marin-ops".to_string(),
            repository_id: None,
            workflow_ref: None,
            event_name: None,
            ref_pattern: None,
            profiles: vec!["actions".to_string()],
        };

        let created = federation_add(&db, &req).await.unwrap();
        let unchanged = federation_add(&db, &req).await.unwrap();
        assert_eq!(unchanged.id, created.id);
        assert_eq!(unchanged.updated_at, created.updated_at);
        assert_eq!(unchanged.subject.as_deref(), Some("11223344556677889900"));
        assert_eq!(unchanged.profiles, vec!["actions"]);

        let mut invalid = req;
        invalid.name = "invalid-google".to_string();
        invalid.subject = Some("service-account-name".to_string());
        assert!(federation_add(&db, &invalid).await.is_err());
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
                name: "actions-main".to_string(),
                provider: "github".to_string(),
                issuer: issuer.clone(),
                audience: "loom-test".to_string(),
                subject: None,
                service_account: None,
                service_tag: "github-actions".to_string(),
                repository_id: Some("123".to_string()),
                workflow_ref: Some(
                    "acme/repo/.github/workflows/loom.yml@refs/heads/main".to_string(),
                ),
                event_name: Some("issues".to_string()),
                ref_pattern: Some("refs/heads/main".to_string()),
                profiles: vec!["actions".to_string()],
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
        let federation = loom.federation.unwrap();
        assert_eq!(federation.service_tag, "github-actions");
        let context = federation.github.unwrap();
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
