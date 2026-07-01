//! GitHub App creation via the **manifest flow** — the engine behind
//! `loom setup github-app`.
//!
//! Registering the App loom needs (webhook receiver + issue-comment/collaborator
//! REST identity, §6.3 of the shared-loom design; see [`crate::github_app`]) is
//! normally a manual, multi-screen "New GitHub App" form
//! ([`docs/github-trigger.md`](../../../docs/github-trigger.md)). GitHub's
//! [manifest flow](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest)
//! collapses that to one confirmation click:
//!
//! 1. loom builds a manifest JSON ([`manifest_json`]) describing the App —
//!    name, homepage, webhook URL, and (via `callback_urls`) the "Sign in with
//!    GitHub" callback, so the App's own OAuth client doubles as loom's login
//!    app (collapsing what would otherwise be two separate GitHub
//!    registrations into one).
//! 2. A local HTML page ([`submission_html`], served at `/` by
//!    [`run_local_server`]) auto-POSTs that manifest to GitHub ([`create_url`]);
//!    the operator's only action is GitHub's own "Create GitHub App"
//!    confirmation.
//! 3. GitHub redirects the browser back to a `redirect_url` loom is listening
//!    on ([`run_local_server`]'s `/callback`) with a one-time `code`.
//! 4. loom exchanges that code for the full credential set — App id, RSA
//!    private key, webhook secret, OAuth client id/secret — via
//!    [`convert`]/`POST /app-manifests/{code}/conversions`, which needs **no
//!    prior authentication**: possessing the code (proof the operator just
//!    confirmed creation in their own browser) is the credential.
//!
//! This module only builds the manifest, drives the local HTTP round-trip, and
//! parses GitHub's response — it does not know where the resulting credentials
//! are stored (the `loom setup github-app` command in `bin/loom.rs` writes them
//! straight into the settings table `env_or_setting` already reads, so they take
//! effect on the *running* daemon with no restart).

use std::collections::HashMap;
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
    /// loom's public base URL (e.g. `https://loom.team.dev`) — becomes the
    /// App's homepage and the base of its webhook and OAuth-callback URLs.
    pub base_url: &'a str,
    /// Where GitHub redirects the browser with the manifest-flow `code` —
    /// loom's local callback server, not `base_url`.
    pub redirect_url: &'a str,
}

/// Build the manifest JSON GitHub's manifest flow expects. Mirrors the
/// hand-registration steps in `docs/github-trigger.md` "Create the App":
/// webhook URL + secret placeholder (GitHub mints the secret itself and
/// returns it in the conversion — no `hook_attributes.secret` needed),
/// Issues/Contents write + Metadata read, subscribed to `issue_comment`, and
/// (new here) `callback_urls` so the App's own client id/secret also serves
/// loom's "Sign in with GitHub" login — the same `/login/oauth/authorize` and
/// `/login/oauth/access_token` endpoints [`crate::auth::github_oauth`] already
/// speaks work unchanged for a GitHub App's user-to-server OAuth.
pub fn manifest_json(input: &ManifestInput) -> Value {
    let base = input.base_url.trim_end_matches('/');
    json!({
        "name": input.name,
        "url": input.base_url,
        "hook_attributes": { "url": format!("{base}/api/github/webhook") },
        "redirect_url": input.redirect_url,
        "callback_urls": [format!("{base}/api/auth/github/callback")],
        "public": false,
        "default_events": ["issue_comment"],
        "default_permissions": {
            "issues": "write",
            "contents": "write",
            "metadata": "read"
        }
    })
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

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

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
    pub webhook_secret: String,
    pub pem: String,
    pub owner: Owner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Owner {
    pub login: String,
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
            base_url: "https://loom.acme.dev",
            redirect_url: Box::leak(redirect_url.to_string().into_boxed_str()),
        }
    }

    // -- manifest shape -------------------------------------------------

    #[test]
    fn manifest_wires_the_webhook_login_and_least_privilege_permissions() {
        let m = manifest_json(&input("http://127.0.0.1:9999/callback"));
        assert_eq!(m["name"], "loom-acme");
        assert_eq!(m["url"], "https://loom.acme.dev");
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://loom.acme.dev/api/github/webhook"
        );
        assert_eq!(m["redirect_url"], "http://127.0.0.1:9999/callback");
        assert_eq!(
            m["callback_urls"][0],
            "https://loom.acme.dev/api/auth/github/callback"
        );
        assert_eq!(m["public"], false);
        assert_eq!(m["default_events"][0], "issue_comment");
        assert_eq!(m["default_permissions"]["issues"], "write");
        assert_eq!(m["default_permissions"]["contents"], "write");
        assert_eq!(m["default_permissions"]["metadata"], "read");
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
            "owner": {"login": "acme"},
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
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
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
        assert_eq!(conv.webhook_secret, "secret-webhook");
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
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();
        let err = convert_at(&client, &base, "stale").await.unwrap_err();
        assert!(err.to_string().contains("code expired"), "{err}");
    }
}
