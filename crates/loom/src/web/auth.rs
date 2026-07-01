use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use weaver_api::{
    AddUserReq, AuthMethods, CreateTokenReq, CreatedTokenView, GithubConfigView, LoginReq, MeView,
    SetGithubConfigReq, SetPasswordReq, TokenView, UserView,
};

use crate::auth::{self, Principal};
use crate::config;

use super::{ApiResult, AppError, AppState};

// ===========================================================================
// Authentication
//
// Three credentials resolve to one `auth::Principal`: an `Authorization: Bearer`
// API token, a login session cookie, or a trusted-loopback request. The
// `require_auth` middleware enforces this on every route except the public login
// surface (`/auth/me`, `/auth/login`, `/auth/logout`, `/auth/github/*`) and
// `/health`. The crypto and storage live in `crate::auth`; this is the HTTP glue.
// ===========================================================================

/// The login cookie's `Max-Age` in seconds, derived from the stored-session TTL
/// so the cookie and the server-side expiry can't drift apart.
const SESSION_MAX_AGE: i64 = auth::SESSION_TTL_DAYS * 24 * 60 * 60;
/// The short-lived cookie carrying the OAuth CSRF state across the round-trip.
const OAUTH_STATE_COOKIE: &str = "loom_oauth_state";
/// The GitHub OAuth callback path — the redirect URI registered on the app and
/// reported to the settings UI.
const GITHUB_CALLBACK_PATH: &str = "/api/auth/github/callback";

fn unauthorized(message: &str) -> AppError {
    AppError::new(StatusCode::UNAUTHORIZED, message)
}

/// Pull the token out of an `Authorization: Bearer <token>` header.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Read one cookie value by name out of the `Cookie` request header.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// Resolve the caller to an authenticated [`Principal`], or `None`. Order: a
/// bearer token, a session cookie, then loopback trust.
async fn resolve_principal(st: &AppState, headers: &HeaderMap, peer: IpAddr) -> Option<Principal> {
    if let Some(token) = bearer_token(headers) {
        if let Ok(Some(p)) = auth::lookup_token(&st.db, &token).await {
            return Some(p);
        }
    }
    if let Some(cookie) = cookie_value(headers, auth::SESSION_COOKIE) {
        if let Ok(Some(p)) = auth::lookup_session(&st.db, &cookie).await {
            return Some(p);
        }
    }
    if peer.is_loopback()
        && config::get_bool(
            &st.db,
            "auth.trust_loopback",
            config::DEFAULT_TRUST_LOOPBACK,
        )
        .await
    {
        if let Ok(Some(p)) = auth::loopback_principal(&st.db).await {
            return Some(p);
        }
    }
    None
}

/// Middleware: reject any request that doesn't resolve to a [`Principal`],
/// otherwise stash it in the request extensions for the handler.
pub(super) async fn require_auth(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let headers = req.headers().clone();
    match resolve_principal(&st, &headers, peer.ip()).await {
        Some(principal) => {
            req.extensions_mut().insert(principal);
            next.run(req).await
        }
        None => unauthorized("authentication required").into_response(),
    }
}

// -- Cookie + redirect helpers ----------------------------------------------

/// Build a `Set-Cookie` value for the login session. `max_age` of 0 clears it.
fn session_cookie(value: &str, max_age: i64, secure: bool) -> String {
    let mut c = format!(
        "{}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}",
        auth::SESSION_COOKIE
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Build the `Set-Cookie` value for the short-lived OAuth state cookie.
fn state_cookie(value: &str, max_age: i64) -> String {
    format!("{OAUTH_STATE_COOKIE}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}")
}

/// A 303 redirect to `location`, appending each given `Set-Cookie` header.
fn redirect_with_cookies(location: &str, cookies: &[String]) -> Response {
    let mut resp = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .body(axum::body::Body::empty())
        .expect("static redirect response is well-formed");
    let h = resp.headers_mut();
    for c in cookies {
        if let Ok(v) = header::HeaderValue::from_str(c) {
            h.append(header::SET_COOKIE, v);
        }
    }
    resp
}

/// Redirect back to the SPA login screen with an error code it can render.
fn login_error_redirect(code: &str) -> Response {
    redirect_with_cookies(&format!("/login?error={code}"), &[])
}

async fn cookie_secure(st: &AppState) -> bool {
    config::get_bool(&st.db, "auth.cookie_secure", config::DEFAULT_COOKIE_SECURE).await
}

/// The externally-visible base URL, for the OAuth callback. Prefers the
/// `auth.base_url` setting; otherwise derives `{proto}://{host}` from the request
/// (honouring `X-Forwarded-Proto` from a TLS-terminating proxy).
pub(crate) async fn external_base(st: &AppState, headers: &HeaderMap) -> Option<String> {
    let configured = config::get(&st.db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if !configured.is_empty() {
        return Some(configured);
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    Some(format!("{proto}://{host}"))
}

// -- Identity ----------------------------------------------------------------

async fn auth_methods(st: &AppState) -> AuthMethods {
    AuthMethods {
        password: true,
        github: auth::github_oauth(&st.db).await.is_some(),
    }
}

/// `GET /api/auth/me` — who the caller is + which sign-in methods to offer.
/// Public: an unauthenticated caller gets `authenticated: false`, not a 401.
pub(super) async fn auth_me(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<MeView> {
    let principal = resolve_principal(&st, &headers, peer.ip()).await;
    let methods = auth_methods(&st).await;
    Json(match principal {
        Some(p) => MeView {
            authenticated: true,
            username: Some(p.username),
            github_login: p.github_login,
            via: Some(p.via.as_str().to_string()),
            methods,
        },
        None => MeView {
            authenticated: false,
            username: None,
            github_login: None,
            via: None,
            methods,
        },
    })
}

/// `POST /api/auth/login` — username/password. Sets the session cookie.
pub(super) async fn auth_login(
    State(st): State<AppState>,
    Json(body): Json<LoginReq>,
) -> ApiResult<Response> {
    let principal = auth::verify_login(&st.db, body.username.trim(), &body.password)
        .await?
        .ok_or_else(|| unauthorized("invalid username or password"))?;
    let cookie = auth::create_session(&st.db, &principal.username).await?;
    let set = session_cookie(&cookie, SESSION_MAX_AGE, cookie_secure(&st).await);
    Ok((
        [(header::SET_COOKIE, set)],
        Json(json!({ "username": principal.username })),
    )
        .into_response())
}

/// `POST /api/auth/logout` — drop the session and clear the cookie.
pub(super) async fn auth_logout(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(cookie) = cookie_value(&headers, auth::SESSION_COOKIE) {
        auth::delete_session(&st.db, &cookie).await.ok();
    }
    let clear = session_cookie("", 0, cookie_secure(&st).await);
    Ok(([(header::SET_COOKIE, clear)], Json(json!({ "ok": true }))).into_response())
}

// -- GitHub OAuth ------------------------------------------------------------

/// `GET /api/auth/github/login` — begin the OAuth dance.
pub(super) async fn github_login(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let cfg = auth::github_oauth(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("GitHub sign-in is not configured"))?;
    let base = external_base(&st, &headers).await.ok_or_else(|| {
        AppError::bad_request("cannot determine the callback URL (no Host header)")
    })?;
    let redirect_uri = format!("{base}{GITHUB_CALLBACK_PATH}");
    let state = auth::random_state();
    let url = auth::authorize_url(&cfg, &state, &redirect_uri);
    Ok(redirect_with_cookies(&url, &[state_cookie(&state, 600)]))
}

#[derive(Debug, Deserialize)]
pub(super) struct GithubCallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

/// `GET /api/auth/github/callback` — finish the dance: verify state, exchange the
/// code, check the GitHub login against the allowlist, open a session.
pub(super) async fn github_callback(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<GithubCallbackQuery>,
) -> ApiResult<Response> {
    let cfg = auth::github_oauth(&st.db)
        .await
        .ok_or_else(|| AppError::bad_request("GitHub sign-in is not configured"))?;
    // CSRF: the returned state must match the cookie we set at /login.
    let expected = cookie_value(&headers, OAUTH_STATE_COOKIE);
    if expected.is_none() || q.state.is_none() || expected != q.state {
        return Ok(login_error_redirect("state-mismatch"));
    }
    let Some(code) = q.code.filter(|c| !c.is_empty()) else {
        return Ok(login_error_redirect("missing-code"));
    };
    let base = external_base(&st, &headers)
        .await
        .ok_or_else(|| AppError::bad_request("cannot determine the callback URL"))?;
    let redirect_uri = format!("{base}{GITHUB_CALLBACK_PATH}");
    let token = auth::exchange_code(&cfg, &code, &redirect_uri).await?;
    let login = auth::fetch_github_login(&token).await?;
    let Some(user) = auth::user_by_github(&st.db, &login).await? else {
        // Authenticated with GitHub, but not on the allowlist.
        return Ok(login_error_redirect("not-approved"));
    };
    let cookie = auth::create_session(&st.db, &user.username).await?;
    Ok(redirect_with_cookies(
        "/",
        &[
            session_cookie(&cookie, SESSION_MAX_AGE, cookie_secure(&st).await),
            state_cookie("", 0),
        ],
    ))
}

// -- API tokens --------------------------------------------------------------

fn token_view(info: auth::TokenInfo) -> TokenView {
    TokenView {
        id: info.id,
        name: info.name,
        prefix: info.prefix,
        created_at: info.created_at,
        last_used_at: info.last_used_at,
        expires_at: info.expires_at,
    }
}

/// `GET /api/auth/tokens` — the user-managed API tokens.
pub(super) async fn list_tokens(State(st): State<AppState>) -> ApiResult<Json<Vec<TokenView>>> {
    let tokens = auth::list_tokens(&st.db).await?;
    Ok(Json(tokens.into_iter().map(token_view).collect()))
}

/// `POST /api/auth/tokens` — mint a token, returning the plaintext once.
pub(super) async fn create_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<CreateTokenReq>,
) -> ApiResult<Json<CreatedTokenView>> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("a token name is required"));
    }
    let (token, info) =
        auth::create_token(&st.db, &principal.username, name, body.expires_in_days).await?;
    Ok(Json(CreatedTokenView {
        token,
        info: token_view(info),
    }))
}

/// `DELETE /api/auth/tokens/{id}` — revoke a token.
pub(super) async fn revoke_token(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if auth::revoke_token(&st.db, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("token"))
    }
}

// -- Account + users ---------------------------------------------------------

/// `POST /api/auth/password` — set/change the caller's own password.
pub(super) async fn set_own_password(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<SetPasswordReq>,
) -> ApiResult<StatusCode> {
    if body.new_password.len() < 8 {
        return Err(AppError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    auth::set_password(&st.db, &principal.username, Some(&body.new_password)).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn user_view(u: auth::User) -> UserView {
    let has_password = u.has_password();
    UserView {
        username: u.username,
        github_login: u.github_login,
        has_password,
        created_at: u.created_at,
    }
}

/// `GET /api/auth/users` — the approved-operator allowlist.
pub(super) async fn list_users(State(st): State<AppState>) -> ApiResult<Json<Vec<UserView>>> {
    let users = auth::list_users(&st.db).await?;
    Ok(Json(users.into_iter().map(user_view).collect()))
}

/// `POST /api/auth/users` — approve a new operator.
pub(super) async fn add_user(
    State(st): State<AppState>,
    Json(body): Json<AddUserReq>,
) -> ApiResult<Json<UserView>> {
    let username = body.username.trim();
    if username.is_empty() {
        return Err(AppError::bad_request("a username is required"));
    }
    let github = body
        .github_login
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = body.password.as_deref().filter(|s| !s.is_empty());
    if github.is_none() && password.is_none() {
        return Err(AppError::bad_request(
            "set a GitHub login or a password so the user can sign in",
        ));
    }
    if let Some(p) = password {
        if p.len() < 8 {
            return Err(AppError::bad_request(
                "password must be at least 8 characters",
            ));
        }
    }
    auth::add_user(&st.db, username, github, password)
        .await
        .map_err(|e| AppError::bad_request(format!("could not add user: {e}")))?;
    let user = auth::get_user(&st.db, username)
        .await?
        .ok_or_else(|| AppError::not_found("user"))?;
    Ok(Json(user_view(user)))
}

/// `DELETE /api/auth/users/{username}` — remove an approved operator.
pub(super) async fn remove_user(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(username): Path<String>,
) -> ApiResult<StatusCode> {
    if username == principal.username {
        return Err(AppError::bad_request("you cannot remove yourself"));
    }
    match auth::remove_user(&st.db, &username).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(AppError::not_found("user")),
        Err(e) => Err(AppError::bad_request(e.to_string())),
    }
}

// -- GitHub OAuth app config -------------------------------------------------

async fn github_config_view(st: &AppState) -> ApiResult<GithubConfigView> {
    let client_id = config::get(&st.db, auth::GH_CLIENT_ID_KEY)
        .await
        .unwrap_or_default();
    Ok(GithubConfigView {
        configured: auth::github_oauth(&st.db).await.is_some(),
        client_id,
        callback_path: GITHUB_CALLBACK_PATH.to_string(),
    })
}

/// `GET /api/auth/github/config` — the GitHub sign-in setup (secret withheld).
pub(super) async fn get_github_config(
    State(st): State<AppState>,
) -> ApiResult<Json<GithubConfigView>> {
    Ok(Json(github_config_view(&st).await?))
}

/// `PUT /api/auth/github/config` — set the OAuth app id (and, optionally, secret).
pub(super) async fn put_github_config(
    State(st): State<AppState>,
    Json(body): Json<SetGithubConfigReq>,
) -> ApiResult<Json<GithubConfigView>> {
    let mut changes: Vec<config::Change> = vec![(
        auth::GH_CLIENT_ID_KEY.to_string(),
        Some(body.client_id.trim().to_string()),
    )];
    // The secret is write-only: a value sets it, an empty string clears it, and
    // omitting the field leaves the stored secret untouched.
    if let Some(secret) = body.client_secret {
        let secret = secret.trim().to_string();
        changes.push((
            auth::GH_CLIENT_SECRET_KEY.to_string(),
            (!secret.is_empty()).then_some(secret),
        ));
    }
    config::apply(&st.db, &changes).await?;
    Ok(Json(github_config_view(&st).await?))
}
