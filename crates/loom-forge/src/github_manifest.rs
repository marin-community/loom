//! Register private Apps for repository access or human authentication through
//! GitHub's browser-confirmed manifest flow. Each purpose has its own permissions
//! and credential namespace, so registering one never replaces the other.

use crate::github_app::GithubAppPurpose;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// The production GitHub REST base.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "loom-setup";

/// Grace period after the callback fires before [`run_local_server`] returns,
/// giving the background server time to actually flush the HTTP response to
/// the browser (the oneshot fires the instant the handler decides the outcome,
/// which happens before the response bytes are written to the socket).
const FLUSH_GRACE: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------

/// What shapes the manifest: everything specific to this loom instance.
pub struct ManifestInput<'a> {
    /// The App's display name. Must be globally unique across GitHub; GitHub
    /// rejects the submission (back on its own form) if it collides.
    pub name: &'a str,
    pub purpose: GithubAppPurpose,
    /// loom's public base URL (e.g. `https://loom.team.dev`) — becomes the
    /// App's homepage and the base of its webhook and OAuth-callback URLs.
    pub base_url: &'a str,
    /// Where GitHub redirects the browser with the manifest-flow `code` —
    /// loom's local callback server, not `base_url`.
    pub redirect_url: &'a str,
}

/// Build a private App manifest with permissions restricted to its purpose.
pub fn manifest_json(input: &ManifestInput) -> Value {
    let base = input.base_url.trim_end_matches('/');
    let mut manifest = json!({
        "name": input.name,
        "url": input.base_url,
        "redirect_url": input.redirect_url,
        "public": false,
    });
    match input.purpose {
        GithubAppPurpose::Repository => {
            manifest["hook_attributes"] = json!({ "url": format!("{base}/api/github/webhook") });
            manifest["default_events"] = json!(["issue_comment", "issues", "pull_request_review"]);
            manifest["default_permissions"] = json!({
                "actions": "write", "contents": "write", "issues": "write",
                "metadata": "read", "pull_requests": "write", "workflows": "write"
            });
        }
        GithubAppPurpose::Authentication => {
            manifest["hook_attributes"] = json!({ "active": false });
            manifest["callback_urls"] = json!([format!("{base}/api/auth/github/callback")]);
            manifest["default_permissions"] = json!({ "members": "read" });
        }
    }
    manifest
}

/// Where the browser is sent to confirm App creation: the org-scoped form when
/// `org` is given, else the personal-account form. `state` is echoed back
/// unchanged on the redirect — [`run_local_server`]'s CSRF guard.
pub fn create_url(org: Option<&str>, state: &str) -> String {
    let state = percent_encoding::utf8_percent_encode(state, percent_encoding::NON_ALPHANUMERIC);
    match org {
        // GitHub org logins are alphanumeric-and-hyphen only, so no encoding
        // is needed for the path segment (and encoding a bare `-` would be
        // wrong: GitHub reads it literally in the org's URL slug).
        Some(org) => {
            format!("https://github.com/organizations/{org}/settings/apps/new?state={state}")
        }
        None => format!("https://github.com/settings/apps/new?state={state}"),
    }
}

/// The GitHub page to edit an existing App's settings and permissions. A
/// personal App lives under the owner's own developer settings; an org-owned App
/// (`org` set) under the organization's. This is where an operator updates
/// permissions loom can't change itself (GitHub App permissions are owner-edited
/// in the UI, then re-approved per installation).
pub fn settings_url(slug: &str, org: Option<&str>) -> String {
    match org.map(str::trim).filter(|s| !s.is_empty()) {
        Some(org) => format!("https://github.com/organizations/{org}/settings/apps/{slug}"),
        None => format!("https://github.com/settings/apps/{slug}"),
    }
}

/// The page to install the App (or adjust which repositories it can access, and
/// re-approve updated permissions) — the public App install page, valid for both
/// personal and org-owned Apps.
pub fn install_url(slug: &str) -> String {
    format!("https://github.com/apps/{slug}/installations/new")
}

/// A local HTML page that auto-submits `manifest` to `target_url` the instant
/// it loads, so the operator's browser lands straight on GitHub's own "Create
/// GitHub App" confirmation screen. GitHub's manifest flow is POST-only (the
/// manifest is a hidden form field, not a query string), so a plain link can't
/// do this — the page exists only to fire that POST.
pub fn submission_html(manifest: &Value, target_url: &str) -> String {
    let manifest_str = serde_json::to_string(manifest).expect("manifest is valid JSON");
    let escaped = html_escape(&manifest_str);
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>loom setup</title></head>
<body>
<p>Redirecting to GitHub to create the App…</p>
<form id="loom-setup-form" action="{target_url}" method="post">
<input type="hidden" name="manifest" value="{escaped}">
</form>
<script>document.getElementById('loom-setup-form').submit();</script>
</body></html>
"#
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// The local callback server
// ---------------------------------------------------------------------------

/// Bind a local listener that serves `start_html` at `/` (the auto-submitting
/// confirmation page from [`submission_html`]) and waits for GitHub's
/// manifest-flow redirect (`GET /callback?code=…&state=…`, `expected_state`
/// echoed back unchanged) at `/callback`, returning the `code`.
///
/// Serving the start page from the *same* listener — rather than writing it to
/// a local file and opening a `file://` URL — means the only requirement on
/// the operator's browser is that it can reach this listener's `host:port`.
/// That's true unmodified for a local run, and for a remote/containerized loom
/// it's exactly what an SSH tunnel (`ssh -L <port>:localhost:<port> …`) or a
/// one-shot published Docker port bridges — no separate step to get the HTML
/// itself in front of the browser.
///
/// Errors (state mismatch, no code, or nothing arrives before `timeout`) all
/// propagate as `Err` with a message meant to be printed directly.
pub async fn run_local_server(
    listener: TcpListener,
    start_html: String,
    expected_state: String,
    timeout: Duration,
) -> Result<String> {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let tx = Arc::new(Mutex::new(Some(tx)));

    let app = axum::Router::new()
        .route("/", get(move || async move { Html(start_html) }))
        .route(
            "/callback",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let tx = tx.clone();
                let expected = expected_state.clone();
                async move {
                    let outcome = match (q.get("code"), q.get("state")) {
                        (Some(code), Some(state)) if *state == expected => Ok(code.clone()),
                        (Some(_), Some(_)) => Err(
                            "GitHub's redirect carried a state that doesn't match this run — \
                             start over with `loom setup github-app`"
                                .to_string(),
                        ),
                        _ => Err(
                            "GitHub's redirect carried no code — App creation may have been \
                             cancelled"
                                .to_string(),
                        ),
                    };
                    let page = match &outcome {
                        Ok(_) => {
                            "<html><body><h3>loom setup: GitHub App created</h3>\
                            <p>You can close this tab and return to the terminal.</p></body></html>"
                        }
                        Err(_) => {
                            "<html><body><h3>loom setup: something went wrong</h3>\
                            <p>Check the terminal for details.</p></body></html>"
                        }
                    };
                    if let Some(sender) = tx.lock().expect("callback mutex poisoned").take() {
                        let _ = sender.send(outcome);
                    }
                    Html(page)
                }
            }),
        );

    weaver_core::spawn_boxed(Box::pin(async move {
        let _ = axum::serve(listener, app).await;
    }));

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok(code))) => {
            // Give the response we just returned a moment to actually reach the
            // browser before the caller moves on (and, in the CLI, the process
            // eventually exits) — see FLUSH_GRACE.
            tokio::time::sleep(FLUSH_GRACE).await;
            Ok(code)
        }
        Ok(Ok(Err(reason))) => bail!(reason),
        Ok(Err(_)) => bail!("the local callback server stopped unexpectedly"),
        Err(_) => bail!(
            "timed out waiting for the GitHub App confirmation in the browser — \
             re-run `loom setup github-app` when you're ready"
        ),
    }
}

// ---------------------------------------------------------------------------
// The manifest-code → credentials exchange
// ---------------------------------------------------------------------------

/// Everything the manifest-conversion endpoint returns that loom needs. GitHub
/// sends more fields (name, description, permissions, events, …); serde drops
/// what we don't name here.
#[derive(Debug, Clone, Deserialize)]
pub struct AppManifestConversion {
    pub id: i64,
    pub slug: String,
    pub html_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub webhook_secret: Option<String>,
    pub pem: String,
    pub owner: Owner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Owner {
    pub login: String,
    pub id: i64,
}

/// Store one App's credentials without changing the other App or its OAuth client.
pub async fn store_app_credentials(
    db: &crate::db::Db,
    config_path: &Path,
    purpose: GithubAppPurpose,
    conversion: &AppManifestConversion,
    organization: Option<&str>,
) -> Result<()> {
    let settings = purpose.settings();
    let app_id = conversion.id.to_string();
    let mut stored = vec![
        (settings.id, app_id.as_str()),
        (settings.private_key, conversion.pem.as_str()),
        (settings.slug, conversion.slug.as_str()),
        (settings.owner, organization.unwrap_or_default()),
    ];
    let mut exported = vec![
        (settings.id_env, app_id.as_str()),
        (settings.private_key_env, conversion.pem.as_str()),
        (settings.slug_env, conversion.slug.as_str()),
    ];
    match purpose {
        GithubAppPurpose::Repository => {
            let webhook_secret = conversion
                .webhook_secret
                .as_deref()
                .context("repository App conversion omitted its webhook secret")?;
            stored.push((crate::github_trigger::WEBHOOK_SECRET_KEY, webhook_secret));
            exported.push(("LOOM_GITHUB_WEBHOOK_SECRET", webhook_secret));
        }
        GithubAppPurpose::Authentication => {
            stored.extend([
                (crate::auth::GH_CLIENT_ID_KEY, conversion.client_id.as_str()),
                (
                    crate::auth::GH_CLIENT_SECRET_KEY,
                    conversion.client_secret.as_str(),
                ),
            ]);
            exported.extend([
                ("LOOM_GITHUB_CLIENT_ID", conversion.client_id.as_str()),
                (
                    "LOOM_GITHUB_CLIENT_SECRET",
                    conversion.client_secret.as_str(),
                ),
            ]);
        }
    }
    weaver_core::config::apply(
        db,
        &stored
            .into_iter()
            .map(|(key, value)| (key.to_string(), Some(value.to_string())))
            .collect::<Vec<_>>(),
    )
    .await
    .context("storing GitHub App credentials")?;
    crate::loom_config::upsert(config_path, &exported)
        .context("writing GitHub App credentials to loom.toml")?;
    Ok(())
}

/// Exchange a manifest-flow `code` for the App's full credential set via the
/// production GitHub API.
pub async fn convert(code: &str) -> Result<AppManifestConversion> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("building the GitHub HTTP client")?;
    convert_at(&client, DEFAULT_API_BASE, code).await
}

/// Exchange a manifest-flow `code` for credentials against `api_base` — the
/// seam tests point at a mock GitHub.
pub async fn convert_at(
    client: &reqwest::Client,
    api_base: &str,
    code: &str,
) -> Result<AppManifestConversion> {
    let url = format!(
        "{}/app-manifests/{code}/conversions",
        api_base.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("exchanging the manifest code with GitHub")?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .context("reading GitHub's manifest-conversion response")?;
    if !status.is_success() {
        bail!(
            "GitHub rejected the manifest code (HTTP {status}): {}",
            body.trim()
        );
    }
    serde_json::from_str(&body).with_context(|| {
        format!(
            "parsing GitHub's manifest-conversion response: {}",
            body.chars().take(500).collect::<String>()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::Json as AxumJson;

    fn input(redirect_url: &str) -> ManifestInput<'static> {
        ManifestInput {
            name: "loom-acme",
            purpose: GithubAppPurpose::Repository,
            base_url: "https://loom.acme.dev",
            redirect_url: Box::leak(redirect_url.to_string().into_boxed_str()),
        }
    }

    // -- manifest shape -------------------------------------------------

    #[test]
    fn repository_manifest_requests_webhooks_without_login_or_membership() {
        let m = manifest_json(&input("http://127.0.0.1:9999/callback"));
        assert_eq!(m["name"], "loom-acme");
        assert_eq!(m["url"], "https://loom.acme.dev");
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://loom.acme.dev/api/github/webhook"
        );
        assert_eq!(m["redirect_url"], "http://127.0.0.1:9999/callback");
        assert!(m.get("callback_urls").is_none());
        assert_eq!(m["public"], false);
        assert_eq!(
            m["default_events"],
            serde_json::json!(["issue_comment", "issues", "pull_request_review"])
        );
        assert_eq!(
            m["default_permissions"],
            serde_json::json!({
                "actions": "write",
                "contents": "write",
                "issues": "write",
                "metadata": "read",
                "pull_requests": "write",
                "workflows": "write"
            })
        );
    }

    #[test]
    fn authentication_manifest_requests_only_membership_and_login() {
        let mut i = input("http://127.0.0.1:9999/callback");
        i.purpose = GithubAppPurpose::Authentication;
        let m = manifest_json(&i);
        assert_eq!(m["public"], false);
        assert_eq!(m["hook_attributes"], json!({ "active": false }));
        assert_eq!(
            m["callback_urls"],
            json!(["https://loom.acme.dev/api/auth/github/callback"])
        );
        assert_eq!(m["default_permissions"], json!({ "members": "read" }));
        assert!(m.get("default_events").is_none());
    }

    #[tokio::test]
    async fn registering_either_app_preserves_the_other_apps_credentials() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loom.toml");
        let repository = AppManifestConversion {
            id: 101,
            slug: "repository-app".to_string(),
            html_url: "https://github.com/apps/repository-app".to_string(),
            client_id: "repository-client".to_string(),
            client_secret: "repository-client-secret".to_string(),
            webhook_secret: Some("repository-webhook".to_string()),
            pem: "repository-key".to_string(),
            owner: Owner {
                login: "code-org".to_string(),
                id: 1,
            },
        };
        let authentication = AppManifestConversion {
            id: 202,
            slug: "login-app".to_string(),
            client_id: "login-client".to_string(),
            client_secret: "login-client-secret".to_string(),
            webhook_secret: None,
            pem: "login-key".to_string(),
            ..repository.clone()
        };
        store_app_credentials(
            &db,
            &path,
            GithubAppPurpose::Repository,
            &repository,
            Some("code-org"),
        )
        .await
        .unwrap();
        store_app_credentials(
            &db,
            &path,
            GithubAppPurpose::Authentication,
            &authentication,
            Some("people-org"),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::github_app::private_key(&db, GithubAppPurpose::Repository)
                .await
                .as_deref(),
            Some("repository-key")
        );
        assert_eq!(
            crate::github_trigger::webhook_secret(&db).await,
            "repository-webhook"
        );

        let replacement = AppManifestConversion {
            id: 303,
            pem: "replacement-repository-key".to_string(),
            ..repository
        };
        store_app_credentials(
            &db,
            &path,
            GithubAppPurpose::Repository,
            &replacement,
            Some("code-org"),
        )
        .await
        .unwrap();
        let oauth = crate::auth::github_oauth(&db).await.unwrap();
        assert_eq!(oauth.client_id, "login-client");
        assert_eq!(oauth.client_secret, "login-client-secret");
        let config = crate::loom_config::load(&path).unwrap();
        assert_eq!(
            config.github_app_private_key.as_deref(),
            Some("replacement-repository-key")
        );
        assert_eq!(
            config.github_auth_app_private_key.as_deref(),
            Some("login-key")
        );
        assert_eq!(config.github_client_id.as_deref(), Some("login-client"));
        let auth_app = crate::github_app::GithubApp::new(db, GithubAppPurpose::Authentication);
        assert_eq!(auth_app.app_id().await, Some(202));
        assert_eq!(auth_app.private_key().await.as_deref(), Some("login-key"));
    }

    #[test]
    fn manifest_strips_a_trailing_slash_from_base_url() {
        let mut i = input("http://127.0.0.1:1/callback");
        i.base_url = "https://loom.acme.dev/";
        let m = manifest_json(&i);
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://loom.acme.dev/api/github/webhook"
        );
    }

    // -- create_url -------------------------------------------------------

    #[test]
    fn create_url_targets_the_personal_form_with_no_org() {
        let url = create_url(None, "abc123");
        assert_eq!(url, "https://github.com/settings/apps/new?state=abc123");
    }

    #[test]
    fn create_url_targets_the_org_form_and_percent_encodes_state() {
        let url = create_url(Some("acme-corp"), "a b");
        assert_eq!(
            url,
            "https://github.com/organizations/acme-corp/settings/apps/new?state=a%20b"
        );
    }

    // -- settings_url / install_url -----------------------------------------

    #[test]
    fn settings_url_personal_when_no_org() {
        assert_eq!(
            settings_url("loom-acme", None),
            "https://github.com/settings/apps/loom-acme"
        );
        // A stored-but-empty owner (personal App) is treated as no org.
        assert_eq!(
            settings_url("loom-acme", Some("")),
            "https://github.com/settings/apps/loom-acme"
        );
    }

    #[test]
    fn settings_url_org_form_when_org_owned() {
        assert_eq!(
            settings_url("loom-acme", Some("acme-corp")),
            "https://github.com/organizations/acme-corp/settings/apps/loom-acme"
        );
    }

    #[test]
    fn install_url_uses_the_public_app_page() {
        assert_eq!(
            install_url("loom-acme"),
            "https://github.com/apps/loom-acme/installations/new"
        );
    }

    // -- submission_html ----------------------------------------------------

    #[test]
    fn submission_html_embeds_the_manifest_and_targets_and_auto_submits() {
        let manifest = json!({"name": "loom \"quoted\" & <tricky>"});
        let target = "https://github.com/settings/apps/new?state=xyz";
        let html = submission_html(&manifest, target);
        assert!(html.contains(&format!(r#"action="{target}""#)));
        assert!(html.contains("name=\"manifest\""));
        assert!(html.contains("&quot;name&quot;"), "html: {html}");
        assert!(html.contains("&lt;tricky&gt;"), "html: {html}");
        assert!(html.contains(".submit()"), "auto-submits: {html}");
    }

    // -- run_local_server -----------------------------------------------------

    async fn bind_local() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("http://{addr}"))
    }

    #[tokio::test]
    async fn serves_the_start_page_at_root() {
        let (listener, base) = bind_local().await;
        let waiter = tokio::spawn(run_local_server(
            listener,
            "<html>start me up</html>".to_string(),
            "the-state".to_string(),
            Duration::from_secs(5),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let body = reqwest::get(&base).await.unwrap().text().await.unwrap();
        assert!(body.contains("start me up"), "{body}");
        // Finish the round so the spawned server task can be joined cleanly.
        let _ = reqwest::get(format!("{base}/callback?code=abc&state=the-state")).await;
        waiter.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn run_local_server_returns_the_code_on_a_matching_state() {
        let (listener, base) = bind_local().await;
        let waiter = tokio::spawn(run_local_server(
            listener,
            String::new(),
            "the-state".to_string(),
            Duration::from_secs(5),
        ));
        // Give the server a moment to start accepting before we hit it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let resp = reqwest::get(format!("{base}/callback?code=abc&state=the-state"))
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let code = waiter.await.unwrap().unwrap();
        assert_eq!(code, "abc");
    }

    #[tokio::test]
    async fn run_local_server_rejects_a_mismatched_state() {
        let (listener, base) = bind_local().await;
        let waiter = tokio::spawn(run_local_server(
            listener,
            String::new(),
            "expected".to_string(),
            Duration::from_secs(5),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = reqwest::get(format!("{base}/callback?code=abc&state=wrong"))
            .await
            .unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("state"), "{err}");
    }

    #[tokio::test]
    async fn run_local_server_times_out_when_nothing_arrives() {
        let (listener, _base) = bind_local().await;
        let err = run_local_server(
            listener,
            String::new(),
            "s".to_string(),
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    // -- convert_at (mock GitHub) ---------------------------------------------

    async fn mock_conversion() -> AxumJson<Value> {
        AxumJson(json!({
            "id": 555,
            "slug": "loom-acme",
            "html_url": "https://github.com/apps/loom-acme",
            "client_id": "Iv1.abc123",
            "client_secret": "secret-client",
            "webhook_secret": "secret-webhook",
            "pem": "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----",
            "owner": {"login": "acme", "id": 303},
            "name": "loom-acme",
            "permissions": {},
            "events": []
        }))
    }

    async fn spawn_mock() -> String {
        let app =
            axum::Router::new().route("/app-manifests/{code}/conversions", post(mock_conversion));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        weaver_core::spawn_boxed(Box::pin(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn convert_at_parses_the_full_credential_set() {
        let base = spawn_mock().await;
        let client = reqwest::Client::new();
        let conv = convert_at(&client, &base, "the-code").await.unwrap();
        assert_eq!(conv.id, 555);
        assert_eq!(conv.slug, "loom-acme");
        assert_eq!(conv.client_id, "Iv1.abc123");
        assert_eq!(conv.client_secret, "secret-client");
        assert_eq!(conv.webhook_secret.as_deref(), Some("secret-webhook"));
        assert!(conv.pem.contains("BEGIN RSA PRIVATE KEY"));
        assert_eq!(conv.owner.login, "acme");
    }

    #[tokio::test]
    async fn convert_at_surfaces_a_non_2xx_body_on_error() {
        async fn mock_error() -> (axum::http::StatusCode, &'static str) {
            (axum::http::StatusCode::NOT_FOUND, "code expired")
        }
        let app = axum::Router::new().route("/app-manifests/{code}/conversions", post(mock_error));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        weaver_core::spawn_boxed(Box::pin(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();
        let err = convert_at(&client, &base, "stale").await.unwrap_err();
        assert!(err.to_string().contains("code expired"), "{err}");
    }
}
