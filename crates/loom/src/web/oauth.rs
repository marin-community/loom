//! Loom's OAuth 2.1 authorization server, the front door for the remote MCP
//! endpoint in [`super::mcp`].
//!
//! An outside client such as the claude.ai connector discovers this server
//! from `/mcp`'s `401`, sends the person to `/oauth/authorize`, and exchanges
//! the returned code at `/oauth/token` for an ordinary Loom API token (see
//! `loom_policy::oauth`). The identity step is Loom's existing sign-in: the
//! consent page reads the login cookie, and a person without one goes through
//! `/login` (and so GitHub sign-in) first.
//!
//! These are hand-mounted protocol routes, not operations: they answer with
//! redirects, HTML and form-encoded bodies whose shapes RFC 6749, RFC 7636,
//! RFC 8414 and RFC 9728 fix. All of them answer `404` unless
//! `connector.enabled` is on. Registering clients is administration, so that
//! part is the ordinary `auth.oauth_clients.*` operations below.

use std::collections::HashMap;

use axum::{
    extract::{rejection::FormRejection, Form, OriginalUri, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use weaver_api::operations::auth::oauth_clients;
use weaver_api::{CreatedOAuthClientView, OAuthClientView, RemoveOAuthClientResult};

use crate::auth;
use crate::config;
use crate::oauth::{self, AuthorizationCode, Client};
use crate::AppState;

use super::auth::{cookie_secure, cookie_value, public_base, redirect_with_cookies};
use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError};

/// Double-submit CSRF cookie for the consent form.
const CONSENT_COOKIE: &str = "loom_oauth_consent";
/// The MCP endpoint path these credentials are for.
pub(super) const MCP_PATH: &str = "/mcp";
pub(super) const PROTECTED_RESOURCE_METADATA_PATH: &str =
    "/.well-known/oauth-protected-resource/mcp";

pub(super) async fn connector_enabled(st: &AppState) -> bool {
    config::get_bool(
        &st.db,
        config::CONNECTOR_ENABLED_KEY,
        config::DEFAULT_CONNECTOR_ENABLED,
    )
    .await
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

// -- Discovery ----------------------------------------------------------------

/// `GET /.well-known/oauth-protected-resource[/mcp]` (RFC 9728): which
/// authorization server issues credentials for `/mcp`.
pub(super) async fn protected_resource(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !connector_enabled(&st).await {
        return not_found();
    }
    let base = public_base(&st, &headers).await;
    Json(json!({
        "resource": format!("{base}{MCP_PATH}"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "resource_name": "Loom",
    }))
    .into_response()
}

/// `GET /.well-known/oauth-authorization-server` (RFC 8414).
pub(super) async fn authorization_server(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !connector_enabled(&st).await {
        return not_found();
    }
    let base = public_base(&st, &headers).await;
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": [
            "client_secret_basic",
            "client_secret_post",
            "none"
        ],
        "authorization_response_iss_parameter_supported": true,
    }))
    .into_response()
}

// -- Authorization endpoint ----------------------------------------------------

/// The authorization request, as it arrives on the query string and again as
/// hidden fields of the consent form.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    resource: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ConsentForm {
    #[serde(flatten)]
    params: AuthorizeParams,
    csrf: String,
    decision: String,
}

/// A request whose client and redirect URI check out. Errors from here on go
/// back to the client; before this, the client cannot be trusted with them.
struct Validated {
    client: Client,
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
}

/// Validate the client and its redirect URI. A failure here is shown to the
/// person rather than sent anywhere, so a bad request cannot redirect them to
/// an unregistered URI.
async fn validate_client(st: &AppState, params: &AuthorizeParams) -> Result<Client, Response> {
    let client_id = params.client_id.as_deref().unwrap_or_default();
    let Some(client) = oauth::get_client(&st.db, client_id)
        .await
        .map_err(|error| error_page(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?
    else {
        return Err(error_page(
            StatusCode::BAD_REQUEST,
            "This application is not registered with Loom. Ask a Loom administrator to add it.",
        ));
    };
    Ok(client)
}

/// The redirect URI this request returns to, or why it may not return at all.
fn chosen_redirect(client: &Client, params: &AuthorizeParams) -> Result<String, &'static str> {
    let redirect_uri = match (
        params.redirect_uri.as_deref(),
        client.redirect_uris.as_slice(),
    ) {
        (Some(uri), _) => uri.to_string(),
        // OAuth allows omitting the URI only when exactly one is registered.
        (None, [only]) => only.clone(),
        (None, _) => return Err("The application did not say where to return you."),
    };
    if !client.allows_redirect(&redirect_uri) {
        return Err(
            "The application asked to return you to an address Loom does not have registered for it.",
        );
    }
    Ok(redirect_uri)
}

/// A PKCE S256 challenge: the base64url SHA-256 of a verifier, 43 characters.
fn is_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Whether a requested RFC 8707 resource names this server's MCP endpoint.
/// Loom's credentials are good for nothing else.
fn resource_matches(base: &str, resource: Option<&str>) -> bool {
    resource.is_none_or(|resource| {
        let resource = resource.trim_end_matches('/');
        resource == format!("{base}{MCP_PATH}") || resource == base
    })
}

async fn validate(
    st: &AppState,
    headers: &HeaderMap,
    params: &AuthorizeParams,
) -> Result<Validated, Response> {
    let client = validate_client(st, params).await?;
    let redirect_uri = chosen_redirect(&client, params)
        .map_err(|message| error_page(StatusCode::BAD_REQUEST, message))?;
    let base = public_base(st, headers).await;
    let back = |error: &str, description: &str| {
        redirect_to_client(
            &redirect_uri,
            &[
                ("error", error),
                ("error_description", description),
                ("iss", &base),
            ],
            params.state.as_deref(),
        )
    };
    if params.response_type.as_deref() != Some("code") {
        return Err(back(
            "unsupported_response_type",
            "only the code flow is supported",
        ));
    }
    let Some(code_challenge) = params
        .code_challenge
        .as_deref()
        .filter(|challenge| is_challenge(challenge))
    else {
        return Err(back(
            "invalid_request",
            "a PKCE S256 code_challenge is required",
        ));
    };
    if params.code_challenge_method.as_deref() != Some("S256") {
        return Err(back(
            "invalid_request",
            "code_challenge_method must be S256",
        ));
    }
    if !resource_matches(&base, params.resource.as_deref()) {
        return Err(back(
            "invalid_target",
            "Loom issues credentials only for its own MCP endpoint",
        ));
    }
    Ok(Validated {
        client,
        redirect_uri,
        code_challenge: code_challenge.to_string(),
        state: params.state.clone(),
    })
}

/// The signed-in person behind the login cookie, if any. A login cookie is
/// always a person's, never a session or automation credential.
async fn signed_in(st: &AppState, headers: &HeaderMap) -> Option<auth::Principal> {
    let cookie = cookie_value(headers, auth::SESSION_COOKIE)?;
    auth::lookup_session(&st.db, &cookie).await.ok().flatten()
}

/// `GET /oauth/authorize` — show the consent page, or send the person to sign
/// in first and come back here.
pub(super) async fn authorize(
    State(st): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    if !connector_enabled(&st).await {
        return not_found();
    }
    let validated = match validate(&st, &headers, &params).await {
        Ok(validated) => validated,
        Err(response) => return response,
    };
    let Some(person) = signed_in(&st, &headers).await else {
        let here = uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/oauth/authorize");
        let encoded =
            percent_encoding::utf8_percent_encode(here, percent_encoding::NON_ALPHANUMERIC);
        return redirect_with_cookies(&format!("/login?redirect={encoded}"), &[]);
    };
    let csrf = auth::random_state();
    let mut response = consent_page(&validated, &params, &person.username, &csrf).into_response();
    let mut cookie =
        format!("{CONSENT_COOKIE}={csrf}; HttpOnly; SameSite=Strict; Path=/oauth; Max-Age=600");
    if cookie_secure(&st).await {
        cookie.push_str("; Secure");
    }
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// `POST /oauth/authorize` — the person's answer from the consent page.
pub(super) async fn decide(
    State(st): State<AppState>,
    headers: HeaderMap,
    form: Result<Form<ConsentForm>, FormRejection>,
) -> Response {
    if !connector_enabled(&st).await {
        return not_found();
    }
    let Ok(Form(form)) = form else {
        return error_page(StatusCode::BAD_REQUEST, "The consent form was malformed.");
    };
    if cookie_value(&headers, CONSENT_COOKIE).as_deref() != Some(form.csrf.as_str()) {
        return error_page(
            StatusCode::BAD_REQUEST,
            "This consent page expired. Start again from the application.",
        );
    }
    let validated = match validate(&st, &headers, &form.params).await {
        Ok(validated) => validated,
        Err(response) => return response,
    };
    let Some(person) = signed_in(&st, &headers).await else {
        return error_page(
            StatusCode::UNAUTHORIZED,
            "Your Loom sign-in ended. Start again from the application.",
        );
    };
    let base = public_base(&st, &headers).await;
    let clear = format!("{CONSENT_COOKIE}=; HttpOnly; SameSite=Strict; Path=/oauth; Max-Age=0");
    if form.decision != "approve" {
        let mut response = redirect_to_client(
            &validated.redirect_uri,
            &[
                ("error", "access_denied"),
                ("error_description", "the person declined"),
                ("iss", &base),
            ],
            validated.state.as_deref(),
        );
        append_cookie(&mut response, &clear);
        return response;
    }
    let approval = AuthorizationCode {
        client_id: validated.client.client_id.clone(),
        username: person.username.clone(),
        redirect_uri: validated.redirect_uri.clone(),
        code_challenge: validated.code_challenge.clone(),
    };
    let code = match oauth::create_code(&st.db, &approval).await {
        Ok(code) => code,
        Err(error) => {
            tracing::error!(%error, "could not record OAuth approval");
            return error_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Loom could not record the approval. Try again.",
            );
        }
    };
    tracing::info!(
        username = %person.username,
        client = %validated.client.client_id,
        "OAuth client approved"
    );
    let mut response = redirect_to_client(
        &validated.redirect_uri,
        &[("code", &code), ("iss", &base)],
        validated.state.as_deref(),
    );
    append_cookie(&mut response, &clear);
    response
}

fn append_cookie(response: &mut Response, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// A 303 to a registered redirect URI with `pairs` (and `state`) appended to
/// whatever query it already has.
fn redirect_to_client(redirect_uri: &str, pairs: &[(&str, &str)], state: Option<&str>) -> Response {
    let Ok(mut url) = reqwest::Url::parse(redirect_uri) else {
        return error_page(StatusCode::BAD_REQUEST, "The return address is invalid.");
    };
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(key, value);
        }
        if let Some(state) = state {
            query.append_pair("state", state);
        }
    }
    redirect_with_cookies(url.as_str(), &[])
}

// -- Token endpoint -------------------------------------------------------------

fn token_error(status: StatusCode, error: &str, description: &str) -> Response {
    let mut response = (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response();
    no_store(&mut response);
    response
}

fn no_store(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

/// Client credentials from `Authorization: Basic` (RFC 6749 §2.3.1, whose
/// parts are form-encoded before base64) or from the body.
fn client_credentials(
    headers: &HeaderMap,
    form: &HashMap<String, String>,
) -> Result<(String, Option<String>), &'static str> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "));
    if let Some(encoded) = basic {
        if form.contains_key("client_secret") {
            return Err("use one client authentication method");
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or("malformed Basic credentials")?;
        let (id, secret) = decoded
            .split_once(':')
            .ok_or("malformed Basic credentials")?;
        let unescape = |value: &str| {
            percent_encoding::percent_decode_str(&value.replace('+', " "))
                .decode_utf8()
                .map(|value| value.into_owned())
                .map_err(|_| "malformed Basic credentials")
        };
        let id = unescape(id)?;
        if form.get("client_id").is_some_and(|body| *body != id) {
            return Err("client_id does not match the Basic credentials");
        }
        return Ok((id, Some(unescape(secret)?)));
    }
    let id = form
        .get("client_id")
        .cloned()
        .ok_or("client_id is required")?;
    Ok((id, form.get("client_secret").cloned()))
}

/// `POST /oauth/token` — exchange a code or a refresh token.
pub(super) async fn token(
    State(st): State<AppState>,
    headers: HeaderMap,
    form: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    if !connector_enabled(&st).await {
        return not_found();
    }
    let Ok(Form(form)) = form else {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the body must be application/x-www-form-urlencoded",
        );
    };
    let (client_id, secret) = match client_credentials(&headers, &form) {
        Ok(credentials) => credentials,
        Err(description) => {
            return token_error(StatusCode::BAD_REQUEST, "invalid_request", description)
        }
    };
    let client = match oauth::get_client(&st.db, &client_id).await {
        Ok(Some(client)) if client.authenticates(secret.as_deref()) => client,
        Ok(_) => {
            return token_error(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "unknown client or wrong credentials",
            )
        }
        Err(error) => {
            tracing::error!(%error, "OAuth client lookup failed");
            return token_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "client lookup failed",
            );
        }
    };
    let base = public_base(&st, &headers).await;
    if !resource_matches(&base, form.get("resource").map(String::as_str)) {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "Loom issues credentials only for its own MCP endpoint",
        );
    }
    let issued = match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => exchange_code(&st, &client, &form).await,
        Some("refresh_token") => {
            let Some(refresh_token) = form.get("refresh_token") else {
                return token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "refresh_token is required",
                );
            };
            match oauth::refresh_tokens(&st.db, &client, refresh_token).await {
                Ok(Some(issued)) => Ok(issued),
                Ok(None) => Err(token_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "the refresh token is invalid, expired, or its user is no longer authorized",
                )),
                Err(error) => Err(server_error(error)),
            }
        }
        _ => Err(token_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "grant_type must be authorization_code or refresh_token",
        )),
    };
    match issued {
        Ok(issued) => {
            let mut response = Json(json!({
                "access_token": issued.access_token,
                "token_type": "Bearer",
                "expires_in": issued.expires_in,
                "refresh_token": issued.refresh_token,
            }))
            .into_response();
            no_store(&mut response);
            response
        }
        Err(response) => response,
    }
}

fn server_error(error: anyhow::Error) -> Response {
    tracing::error!(error = ?error, "OAuth token issue failed");
    token_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "server_error",
        "could not issue tokens",
    )
}

async fn exchange_code(
    st: &AppState,
    client: &Client,
    form: &HashMap<String, String>,
) -> Result<oauth::IssuedTokens, Response> {
    let invalid_grant =
        |description: &str| token_error(StatusCode::BAD_REQUEST, "invalid_grant", description);
    let (Some(code), Some(verifier)) = (form.get("code"), form.get("code_verifier")) else {
        return Err(token_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "code and code_verifier are required",
        ));
    };
    let Some(grant) = oauth::redeem_code(&st.db, code)
        .await
        .map_err(server_error)?
    else {
        return Err(invalid_grant(
            "the code is invalid, expired, or already used",
        ));
    };
    if grant.client_id != client.client_id {
        return Err(invalid_grant("the code was issued to another client"));
    }
    if form
        .get("redirect_uri")
        .is_some_and(|uri| *uri != grant.redirect_uri)
    {
        return Err(invalid_grant(
            "redirect_uri does not match the authorization request",
        ));
    }
    if !oauth::pkce_matches(verifier, &grant.code_challenge) {
        return Err(invalid_grant(
            "the code_verifier does not match the challenge",
        ));
    }
    match oauth::issue_tokens(&st.db, client, &grant.username).await {
        Ok(issued) => {
            tracing::info!(
                username = %grant.username,
                client = %client.client_id,
                "OAuth tokens issued"
            );
            Ok(issued)
        }
        Err(error) => {
            tracing::warn!(%error, username = %grant.username, "OAuth exchange refused");
            Err(invalid_grant("the person is no longer authorized"))
        }
    }
}

// -- Pages ----------------------------------------------------------------------

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

const PAGE_STYLE: &str =
    "body{font:15px/1.5 system-ui,sans-serif;background:#f6f7f9;color:#1d2330;\
display:flex;min-height:100vh;align-items:center;justify-content:center;margin:0}\
main{background:#fff;border:1px solid #dde1e8;border-radius:10px;padding:28px;max-width:440px}\
h1{font-size:18px;margin:0 0 12px}ul{padding-left:20px}p.who{color:#5b6475;font-size:13px}\
.actions{display:flex;gap:10px;margin-top:20px}button{font:inherit;padding:8px 16px;\
border-radius:6px;border:1px solid #c6ccd6;background:#fff;cursor:pointer}\
button.primary{background:#2d5bd7;border-color:#2d5bd7;color:#fff}";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} · Loom</title><style>{PAGE_STYLE}</style></head>\
         <body><main>{body}</main></body></html>",
        title = escape(title),
    )
}

/// HTML that must never render inside another site's frame or be cached.
fn html_response(status: StatusCode, html: String) -> Response {
    let mut response = (status, Html(html)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn error_page(status: StatusCode, message: &str) -> Response {
    html_response(
        status,
        page(
            "Authorization failed",
            &format!("<h1>Authorization failed</h1><p>{}</p>", escape(message)),
        ),
    )
}

fn consent_page(
    validated: &Validated,
    params: &AuthorizeParams,
    username: &str,
    csrf: &str,
) -> Response {
    let hidden = |name: &str, value: Option<&str>| {
        value.map_or(String::new(), |value| {
            format!(
                "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                escape(value)
            )
        })
    };
    let destination = reqwest::Url::parse(&validated.redirect_uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();
    let body = format!(
        "<h1>Allow {client} to use Loom for you?</h1>\
         <p class=\"who\">Signed in as {user}. You will return to {destination}.</p>\
         <p>{client} will be able to:</p>\
         <ul><li>start Loom sessions on repositories you can use,</li>\
         <li>read your sessions' status, messages and links,</li>\
         <li>send them instructions, interrupt them, and archive them.</li></ul>\
         <p>It acts with your ordinary user permissions, never administrator ones, \
         and loses access when your Loom access ends.</p>\
         <form method=\"post\" action=\"/oauth/authorize\">{fields}\
         <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\
         <div class=\"actions\">\
         <button class=\"primary\" type=\"submit\" name=\"decision\" value=\"approve\">Allow</button>\
         <button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button></div></form>",
        client = escape(&validated.client.name),
        user = escape(username),
        destination = escape(&destination),
        csrf = escape(csrf),
        fields = [
            hidden("response_type", params.response_type.as_deref()),
            hidden("client_id", Some(&validated.client.client_id)),
            hidden("redirect_uri", Some(&validated.redirect_uri)),
            hidden("code_challenge", Some(&validated.code_challenge)),
            hidden("code_challenge_method", params.code_challenge_method.as_deref()),
            hidden("state", params.state.as_deref()),
            hidden("resource", params.resource.as_deref()),
        ]
        .concat(),
    );
    html_response(StatusCode::OK, page("Authorize", &body))
}

// -- Client administration --------------------------------------------------------

fn client_view(client: Client) -> OAuthClientView {
    OAuthClientView {
        confidential: client.is_confidential(),
        client_id: client.client_id,
        name: client.name,
        redirect_uris: client.redirect_uris,
        created_by: client.created_by,
        created_at: client.created_at,
    }
}

async fn create_client_op(
    context: OperationContext,
    input: oauth_clients::create::Input,
) -> ApiResult<CreatedOAuthClientView> {
    let (client, client_secret) = oauth::create_client(
        &context.state.db,
        &input.name,
        &input.redirect_uris,
        !input.public,
        &context.principal.username,
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    tracing::info!(
        username = %context.principal.username,
        client = %client.client_id,
        "OAuth client registered"
    );
    Ok(CreatedOAuthClientView {
        client_secret,
        client: client_view(client),
    })
}

async fn list_clients_op(
    context: OperationContext,
    _input: oauth_clients::list::Input,
) -> ApiResult<Vec<OAuthClientView>> {
    Ok(oauth::list_clients(&context.state.db)
        .await?
        .into_iter()
        .map(client_view)
        .collect())
}

async fn remove_client_op(
    context: OperationContext,
    input: oauth_clients::remove::Input,
) -> ApiResult<RemoveOAuthClientResult> {
    let revoked = oauth::remove_client(&context.state.db, &input.client_id)
        .await?
        .ok_or_else(|| AppError::not_found("OAuth client"))?;
    tracing::info!(
        username = %context.principal.username,
        client = %input.client_id,
        revoked,
        "OAuth client removed"
    );
    Ok(RemoveOAuthClientResult {
        removed: true,
        client_id: input.client_id,
        revoked_credentials: revoked,
    })
}

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<oauth_clients::create::Op, _, _>(create_client_op),
        register::<oauth_clients::list::Op, _, _>(list_clients_op),
        register::<oauth_clients::remove::Op, _, _>(remove_client_op),
    ]
}

/// The JSON a `401` from `/mcp` points at, kept here with the rest of the
/// discovery documents.
pub(super) fn resource_metadata_url(base: &str) -> String {
    format!("{base}{PROTECTED_RESOURCE_METADATA_PATH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_must_name_this_mcp_endpoint() {
        let base = "https://loom.example";
        assert!(resource_matches(base, None));
        assert!(resource_matches(base, Some("https://loom.example/mcp")));
        assert!(resource_matches(base, Some("https://loom.example/mcp/")));
        assert!(resource_matches(base, Some("https://loom.example")));
        assert!(!resource_matches(base, Some("https://evil.example/mcp")));
        assert!(!resource_matches(base, Some("https://loom.example/api")));
    }

    #[test]
    fn challenges_are_s256_shaped() {
        assert!(is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!is_challenge("plain-verifier"));
        assert!(!is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw+cM"));
    }

    #[test]
    fn basic_credentials_are_form_decoded_and_exclusive() {
        let mut headers = HeaderMap::new();
        let encoded = base64::engine::general_purpose::STANDARD.encode("loom-abc:s%2Bcret");
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {encoded}")).unwrap(),
        );
        let mut form = HashMap::new();
        assert_eq!(
            client_credentials(&headers, &form).unwrap(),
            ("loom-abc".to_string(), Some("s+cret".to_string()))
        );
        form.insert("client_secret".to_string(), "x".to_string());
        assert!(client_credentials(&headers, &form).is_err());

        let mut body = HashMap::new();
        body.insert("client_id".to_string(), "loom-public".to_string());
        assert_eq!(
            client_credentials(&HeaderMap::new(), &body).unwrap(),
            ("loom-public".to_string(), None)
        );
    }

    #[test]
    fn redirects_keep_the_registered_query_and_add_state() {
        let response = redirect_to_client(
            "https://claude.ai/api/mcp/auth_callback?x=1",
            &[("code", "c d")],
            Some("s1"),
        );
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://claude.ai/api/mcp/auth_callback?x=1&code=c+d&state=s1"
        );
    }

    #[test]
    fn consent_text_is_escaped() {
        assert_eq!(
            escape("<script>\"x\"&'y'</script>"),
            "&lt;script&gt;&quot;x&quot;&amp;&#39;y&#39;&lt;/script&gt;"
        );
    }
}
