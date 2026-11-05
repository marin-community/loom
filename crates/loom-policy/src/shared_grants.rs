//! Audience-bound shared grants verified locally and against the live authority.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

const AUDIENCE: &str = "loom";
const GRANT_VERSION: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActingPrincipal {
    pub sub: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Permission {
    pub service: String,
    pub actions: Vec<String>,
    pub resource: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedGrantClaims {
    pub grant_version: u64,
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub act: ActingPrincipal,
    pub owner: String,
    pub grant_id: String,
    pub permissions: Vec<Permission>,
    pub policy_revision: String,
    pub iat: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
    pub exp: u64,
    pub jti: String,
}

impl SharedGrantClaims {
    pub fn allows(&self, action: &str, resource: &str) -> bool {
        canonical_resource(resource, false)
            && self.permissions.iter().any(|permission| {
                permission.service == AUDIENCE
                    && permission.actions.iter().any(|allowed| allowed == action)
                    && resource.starts_with(&permission.resource)
            })
    }
}

fn canonical_resource(resource: &str, prefix: bool) -> bool {
    let path = if resource.starts_with('/') {
        resource
    } else if let Some(remainder) = resource.strip_prefix("s3://") {
        let Some((bucket, _)) = remainder.split_once('/') else {
            return false;
        };
        if !bucket
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            || !bucket.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
            })
        {
            return false;
        }
        &resource[5 + bucket.len()..]
    } else {
        return false;
    };
    !path.contains("//")
        && !resource.chars().any(|character| {
            character <= '\u{20}' || matches!(character, '%' | '\\' | '*' | '?' | '#')
        })
        && !path.split('/').any(|segment| matches!(segment, "." | ".."))
        && (!prefix || resource.ends_with('/'))
}

/// Holds public keys only. The configured issuer never comes from a token URL.
pub struct SharedGrantVerifier {
    keys: Vec<DecodingKey>,
    validation: Validation,
}

#[derive(Deserialize)]
struct LiveDecision {
    grant_id: String,
    owner: String,
    actor: String,
    expires_at: f64,
    policy_revision: String,
}

impl SharedGrantVerifier {
    pub fn new(issuer: &str, public_keys: &[String]) -> Result<Self> {
        if issuer.is_empty() || public_keys.is_empty() {
            bail!("shared grants require an issuer and trusted public keys");
        }
        let keys = public_keys
            .iter()
            .map(|pem| {
                DecodingKey::from_ed_pem(pem.as_bytes()).context("invalid shared grant public key")
            })
            .collect::<Result<Vec<_>>>()?;
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[issuer]);
        validation.set_audience(&[AUDIENCE]);
        validation.set_required_spec_claims(&["exp", "iat", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 0;
        Ok(Self { keys, validation })
    }

    pub fn verify(&self, token: &str) -> Result<SharedGrantClaims> {
        let header = decode_header(token).context("invalid shared grant JWT")?;
        if header.alg != Algorithm::EdDSA || header.typ.as_deref() != Some("at+jwt") {
            bail!("expected an EdDSA grant access token");
        }
        let claims = self
            .keys
            .iter()
            .find_map(|key| decode::<SharedGrantClaims>(token, key, &self.validation).ok())
            .ok_or_else(|| anyhow!("shared grant signature or registered claims are invalid"))?
            .claims;
        let now = chrono::Utc::now().timestamp() as u64;
        if claims.grant_version != GRANT_VERSION
            || claims.aud != AUDIENCE
            || claims.sub.is_empty()
            || claims.act.sub != claims.sub
            || claims.owner.is_empty()
            || claims.grant_id.is_empty()
            || claims.policy_revision.is_empty()
            || claims.jti.is_empty()
            || claims.iat > now
            || claims.exp <= claims.iat
            || claims.permissions.is_empty()
            || claims.permissions.iter().any(|permission| {
                permission.service != AUDIENCE
                    || permission.actions.is_empty()
                    || permission
                        .actions
                        .iter()
                        .any(|action| action.is_empty() || action.contains('*'))
                    || !canonical_resource(&permission.resource, true)
            })
        {
            bail!("shared grant identity or permission bounds are invalid");
        }
        Ok(claims)
    }

    /// The supplied HTTP client must disable redirects to keep the bearer at this authority.
    pub async fn authorize(
        &self,
        client: &reqwest::Client,
        authorize_url: &str,
        token: &str,
        action: &str,
        resource: &str,
    ) -> Result<SharedGrantClaims> {
        let url =
            reqwest::Url::parse(authorize_url).context("invalid shared grant authority URL")?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("shared grant authority requires HTTPS outside loopback");
        }
        let claims = self.verify(token)?;
        if !claims.allows(action, resource) {
            bail!("operation exceeds shared grant bounds");
        }
        let response = client
            .post(url)
            .timeout(Duration::from_secs(10))
            .bearer_auth(token)
            .json(
                &serde_json::json!({"audience": AUDIENCE, "action": action, "resource": resource}),
            )
            .send()
            .await
            .context("shared grant authority unavailable")?;
        if response.status() != reqwest::StatusCode::OK {
            bail!("shared grant authority denied this operation");
        }
        let live = response
            .json::<LiveDecision>()
            .await
            .context("invalid shared grant authority decision")?;
        if live.grant_id != claims.grant_id
            || live.owner != claims.owner
            || live.actor != claims.sub
            || live.policy_revision != claims.policy_revision
            || !live.expires_at.is_finite()
            || live.expires_at < claims.exp as f64
            || live.expires_at <= chrono::Utc::now().timestamp() as f64
        {
            bail!("shared grant authority decision does not match the token");
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Corpus {
        issuer: String,
        public_keys: Vec<String>,
        vectors: Vec<Vector>,
        operations: Vec<Operation>,
    }

    #[derive(Deserialize)]
    struct Vector {
        name: String,
        token: String,
        valid: bool,
    }

    #[derive(Deserialize)]
    struct Operation {
        action: String,
        resource: String,
        allowed: bool,
    }

    #[test]
    fn signed_shared_contract_vectors_match_python_verifier() {
        let corpus: Corpus =
            serde_json::from_str(include_str!("../tests/fixtures/shared_grants.json")).unwrap();
        let verifier = SharedGrantVerifier::new(&corpus.issuer, &corpus.public_keys).unwrap();
        for vector in corpus.vectors {
            let verified = verifier.verify(&vector.token);
            assert_eq!(verified.is_ok(), vector.valid, "{}", vector.name);
            let Ok(claims) = verified else { continue };
            assert_eq!(claims.sub, "loom:run-one");
            assert_eq!(claims.owner, "user:power");
            for operation in &corpus.operations {
                assert_eq!(
                    claims.allows(&operation.action, &operation.resource),
                    operation.allowed,
                    "{}",
                    vector.name
                );
            }
        }
    }

    #[tokio::test]
    async fn configured_private_ca_authenticates_both_broker_endpoints() {
        use std::process::{Command, Stdio};
        use tokio::io::AsyncBufReadExt;

        let directory = tempfile::tempdir().unwrap();
        let openssl = |arguments: &[&str]| {
            let output = Command::new("openssl")
                .args(arguments)
                .current_dir(directory.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
            "-days",
            "1",
            "-subj",
            "/CN=Isolated broker CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ]);
        openssl(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
            "-subj",
            "/CN=localhost",
        ]);
        std::fs::write(directory.path().join("server.ext"), "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n").unwrap();
        openssl(&[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "server.crt",
            "-days",
            "1",
            "-extfile",
            "server.ext",
        ]);
        let script = r#"
import https from 'node:https';
import {readFileSync} from 'node:fs';
const server = https.createServer({cert:readFileSync(process.argv[1]), key:readFileSync(process.argv[2])}, (request,response) => {
  let body='';
  request.on('data', chunk => body+=chunk);
  request.on('end', () => {
    const input=JSON.parse(body);
    if (request.method!=='POST' || !request.headers.authorization?.startsWith('Bearer ')
        || input.resource!=='/power/run' || !['/authorize','/credentials'].includes(request.url)) {
      response.writeHead(403).end(); return;
    }
    response.writeHead(200, {'content-type':'application/json'});
    response.end(JSON.stringify({grant_id:'grant-one',owner:'user:power',actor:'loom:run-one',
      expires_at:4102444800,policy_revision:'revision-one',storage_token:'private-endpoint-credential'}));
  });
});
server.listen(0,'127.0.0.1',()=>process.stdout.write(String(server.address().port)+'\n'));
"#;
        let mut server = tokio::process::Command::new("node")
            .args(["--input-type=module", "-e", script, "--"])
            .arg(directory.path().join("server.crt"))
            .arg(directory.path().join("server.key"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut port = String::new();
        let mut output = tokio::io::BufReader::new(server.stdout.take().unwrap());
        tokio::time::timeout(Duration::from_secs(10), output.read_line(&mut port))
            .await
            .unwrap()
            .unwrap();
        let base = format!("https://127.0.0.1:{}", port.trim().parse::<u16>().unwrap());
        let corpus: Corpus =
            serde_json::from_str(include_str!("../tests/fixtures/shared_grants.json")).unwrap();
        let token = &corpus
            .vectors
            .iter()
            .find(|vector| vector.valid)
            .unwrap()
            .token;
        let mut config = crate::auth::SharedGrantConfig {
            issuer: corpus.issuer,
            public_keys: corpus.public_keys,
            authorize_url: format!("{base}/authorize"),
            credentials_url: format!("{base}/credentials"),
            ca_file: None,
            profiles: Default::default(),
        };
        let verifier = SharedGrantVerifier::new(&config.issuer, &config.public_keys).unwrap();
        assert!(verifier
            .authorize(
                &config.client().unwrap(),
                &config.authorize_url,
                token,
                "session.create",
                "/power/run"
            )
            .await
            .is_err());
        config.ca_file = Some(directory.path().join("ca.crt"));
        let client = config.client().unwrap();
        let granted = verifier
            .authorize(
                &client,
                &config.authorize_url,
                token,
                "session.create",
                "/power/run",
            )
            .await
            .unwrap();
        assert_eq!(granted.grant_id, "grant-one");
        let credentials = client
            .post(&config.credentials_url)
            .bearer_auth(token)
            .json(&serde_json::json!({"resource":"/power/run"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(credentials["storage_token"], "private-endpoint-credential");
        server.kill().await.unwrap();
    }

    #[tokio::test]
    async fn live_authority_revocation_and_mismatched_identity_are_denied() {
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let corpus: Corpus =
            serde_json::from_str(include_str!("../tests/fixtures/shared_grants.json")).unwrap();
        let token = corpus
            .vectors
            .iter()
            .find(|vector| vector.valid)
            .unwrap()
            .token
            .clone();
        let decision = Arc::new(Mutex::new(Some(serde_json::json!({
            "grant_id": "grant-one", "owner": "user:power", "actor": "loom:run-one",
            "policy_revision": "revision-one", "expires_at": 4102444800.0
        }))));
        async fn authorize(
            State(decision): State<Arc<Mutex<Option<serde_json::Value>>>>,
            headers: HeaderMap,
            Json(operation): Json<serde_json::Value>,
        ) -> Result<Json<serde_json::Value>, StatusCode> {
            if !headers
                .get("authorization")
                .is_some_and(|value| value.as_bytes().starts_with(b"Bearer "))
                || operation
                    != serde_json::json!({"audience": "loom", "action": "session.create", "resource": "/power/run"})
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            decision
                .lock()
                .unwrap()
                .clone()
                .map(Json)
                .ok_or(StatusCode::FORBIDDEN)
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/authorize", post(authorize))
            .with_state(decision.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = format!("http://{address}/v1/authorize");
        let verifier = SharedGrantVerifier::new(&corpus.issuer, &corpus.public_keys).unwrap();
        let claims = verifier
            .authorize(&client, &url, &token, "session.create", "/power/run")
            .await
            .unwrap();
        assert_eq!(claims.grant_id, "grant-one");
        decision.lock().unwrap().as_mut().unwrap()["actor"] = serde_json::json!("loom:run-two");
        assert!(verifier
            .authorize(&client, &url, &token, "session.create", "/power/run")
            .await
            .is_err());
        *decision.lock().unwrap() = None;
        assert!(verifier
            .authorize(&client, &url, &token, "session.create", "/power/run")
            .await
            .is_err());
        server.abort();
    }
}
