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
use sqlx::Row;
use weaver_api::operations::auth::{
    automation_token, federations, github_config, github_token, me, set_password, tokens, users,
};
use weaver_api::{
    AddUserReq, AuthMethods, AutomationTokenView, CreateTokenReq, CreatedTokenView, FederationReq,
    FederationView, GithubConfigView, GithubTokenStatusView, LoginReq, MeView,
    RemoveFederationResult, RemoveUserResult, RevokeTokenResult, SetGithubConfigReq,
    SetPasswordReq, SetUserRoleReq, TokenView, UserRole, UserView,
};

use crate::auth::{self, Grant, Principal};
use crate::config;
use crate::user_token;

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

// ===========================================================================
// Authentication
//
// Three credentials resolve to one `auth::Principal`: an `Authorization: Bearer`
// API token, a login session cookie, or a trusted-loopback request. The
// `require_auth` middleware enforces this on every route except the public login
// surface (`/auth/me`, `/auth/login`, `/auth/logout`, `/auth/github/*`), the
// cryptographically authenticated federation/webhook routes, and
// health/readiness probes. The root `/metrics` route is outside this nested API
// middleware entirely. The crypto and storage live in `crate::auth`; this is
// the HTTP glue.
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

pub(super) async fn is_session_descendant(st: &AppState, ancestor: &str, candidate: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "WITH RECURSIVE tree(id) AS (
           SELECT id FROM sessions WHERE id = ?
           UNION ALL
           SELECT child.id FROM sessions child JOIN tree ON child.parent_session_id = tree.id
         )
         SELECT EXISTS(SELECT 1 FROM tree WHERE id = ?)",
    )
    .bind(ancestor)
    .bind(candidate)
    .fetch_one(&st.db)
    .await
    .unwrap_or(false)
}

pub(super) async fn branch_belongs_to_session_tree(
    st: &AppState,
    ancestor: &str,
    branch_id: &str,
) -> bool {
    sqlx::query_scalar::<_, bool>(
        "WITH RECURSIVE tree(id, branch_id) AS (
           SELECT id, branch_id FROM sessions WHERE id = ?
           UNION ALL
           SELECT child.id, child.branch_id
           FROM sessions child JOIN tree ON child.parent_session_id = tree.id
         )
         SELECT EXISTS(SELECT 1 FROM tree WHERE branch_id = ?)",
    )
    .bind(ancestor)
    .bind(branch_id)
    .fetch_one(&st.db)
    .await
    .unwrap_or(false)
}

async fn issue_belongs_to_session(st: &AppState, branch_id: &str, issue_id: &str) -> bool {
    let Ok(issue_id) = issue_id.parse::<i64>() else {
        return false;
    };
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
           SELECT 1 FROM issues i JOIN branches b ON b.id = ?
           WHERE i.id = ? AND i.repo_root = b.repo_root
         )",
    )
    .bind(branch_id)
    .bind(issue_id)
    .fetch_one(&st.db)
    .await
    .unwrap_or(false)
}

async fn channel_belongs_to_session_tree(st: &AppState, ancestor: &str, channel_id: &str) -> bool {
    let row = sqlx::query(
        "SELECT c.session_id,
                EXISTS(
                  SELECT 1 FROM channel_subscriptions sub
                  WHERE sub.channel_id = c.id
                    AND sub.subject_kind = 'session'
                    AND sub.subject_id = ?
                ) AS subscribed
         FROM channels c WHERE c.id = ?",
    )
    .bind(ancestor)
    .bind(channel_id)
    .fetch_optional(&st.db)
    .await;
    let Ok(Some(row)) = row else {
        return false;
    };
    if row.get::<bool, _>("subscribed") {
        return true;
    }
    match row.get::<Option<String>, _>("session_id") {
        Some(session_id) => is_session_descendant(st, ancestor, &session_id).await,
        None => false,
    }
}

async fn automation_owns_session(st: &AppState, subject: &str, session_id: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sessions
         WHERE id = ? AND creator_kind = 'automation' AND creator_subject = ?)",
    )
    .bind(session_id)
    .bind(subject)
    .fetch_one(&st.db)
    .await
    .unwrap_or(false)
}

/// Whether `principal`'s grant permits `method raw_path`. The router runs this
/// over every request; the multiplexed `/api/events` stream re-runs it per topic
/// against the route that topic stands in for, so folding streams onto one
/// connection cannot widen what a scoped credential can read.
pub(super) async fn grant_allows(
    st: &AppState,
    principal: &Principal,
    method: &axum::http::Method,
    raw_path: &str,
) -> bool {
    if principal.is_admin() {
        return true;
    }
    // A registered operation carries its own authority, and it is complete:
    // `actor` here, then `grants` and the resource named by `Scoped` inside
    // `authorize()` at the dispatcher. The path allowlist below is the *legacy*
    // model. Running both means every newly declared operation is refused until
    // someone also adds its URL to a hand-maintained list — which is exactly the
    // duplicated authority this registry exists to remove, and it is what made
    // `permissions.requests.create` return 403 to the session it was declared
    // for.
    if let Some(operation) = weaver_api::operation_for_request(method.as_str(), raw_path) {
        return operation_grant_allows(principal, operation);
    }
    let path = raw_path.strip_prefix("/api").unwrap_or(raw_path);
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    // The multiplexed stream carries no authority of its own: it is a container
    // whose every topic is re-checked here against the single-stream route it
    // stands in for. Reaching the route grants nothing; the topic list does.
    if *method == axum::http::Method::GET && path == "/events" {
        return true;
    }
    match &principal.grant {
        // No credential reaches a raw path. Anonymous operations are permitted
        // by `operation_grant_allows` on the strength of their declaration, not
        // by a path prefix.
        Grant::Anonymous => false,
        Grant::Admin => true,
        Grant::User => user_grant_allows(method, path),
        Grant::Automation { subject, .. } => {
            if path == "/runs" || path.starts_with("/runs/") {
                return true;
            }
            if *method == axum::http::Method::GET && segments.first() == Some(&"sessions") {
                if let Some(session_id) = segments.get(1) {
                    return automation_owns_session(st, subject, session_id).await;
                }
            }
            false
        }
        Grant::Session {
            session_id,
            branch_id,
            ..
        } => {
            if *method == axum::http::Method::GET
                && matches!(path, "/meta" | "/operations" | "/openapi.json")
            {
                return true;
            }
            if *method == axum::http::Method::GET && path.starts_with("/operations/") {
                return true;
            }
            if *method == axum::http::Method::GET && path == "/self" {
                return true;
            }
            if *method == axum::http::Method::POST && path == "/sessions" {
                return true;
            }
            // `loom session launch` performs this read-only canonical preflight
            // before POSTing `/sessions`. Session principals already have the
            // right to delegate a child launch; letting them resolve the exact
            // template snapshot does not grant another read or write surface.
            if *method == axum::http::Method::POST && path == "/session-launches/resolve" {
                return true;
            }
            if path == "/channels" {
                return matches!(*method, axum::http::Method::GET | axum::http::Method::POST);
            }
            if segments.first() == Some(&"channels") && segments.len() >= 2 {
                return channel_belongs_to_session_tree(st, session_id, segments[1]).await;
            }
            if segments.first() == Some(&"sessions") && segments.len() >= 2 {
                return is_session_descendant(st, session_id, segments[1]).await;
            }
            if segments.first() == Some(&"branches") && segments.len() >= 2 {
                return branch_belongs_to_session_tree(st, session_id, segments[1]).await;
            }
            if segments.first() == Some(&"issues") && segments.len() >= 2 {
                if segments[1] == "actions" && *method == axum::http::Method::POST {
                    return true;
                }
                return issue_belongs_to_session(st, branch_id, segments[1]).await;
            }
            *method == axum::http::Method::GET
                && matches!(
                    path,
                    "/sessions"
                        | "/branches"
                        | "/issues"
                        | "/agents"
                        | "/repos"
                        | "/repos/recent"
                        | "/repos/branches"
                        | "/settings"
                        | "/profiles"
                )
                || (path == "/repos/issues"
                    && matches!(*method, axum::http::Method::GET | axum::http::Method::POST))
        }
    }
}

/// Enforce the operation registry at the API boundary. CLI and MCP adapters
/// therefore cannot widen authority by choosing a different transport.
pub(super) fn operation_grant_allows(
    principal: &Principal,
    operation: &weaver_api::OperationSpec,
) -> bool {
    if operation.actor == weaver_api::ActorPolicy::Anonymous {
        return true;
    }
    match &principal.grant {
        Grant::Anonymous => false,
        // `SessionOnly` returns session credential material, so an operator may
        // not stand in for the session the way `SessionSelf` allows.
        Grant::Admin => operation.actor != weaver_api::ActorPolicy::SessionOnly,
        Grant::User => matches!(
            operation.actor,
            weaver_api::ActorPolicy::SessionSelf | weaver_api::ActorPolicy::User
        ),
        Grant::Automation { .. } => operation.actor == weaver_api::ActorPolicy::Internal,
        Grant::Session { capabilities, .. } => {
            matches!(
                operation.actor,
                weaver_api::ActorPolicy::SessionSelf | weaver_api::ActorPolicy::SessionOnly
            ) && capabilities.as_ref().is_none_or(|granted| {
                operation
                    .grants
                    .iter()
                    .all(|required| granted.iter().any(|value| value == required))
            })
        }
    }
}

fn user_grant_allows(method: &axum::http::Method, path: &str) -> bool {
    if path == "/auth/users"
        || path.starts_with("/auth/users/")
        || path == "/auth/github/config"
        || path == "/auth/automation-token"
        || path == "/auth/federations"
        || path.starts_with("/auth/federations/")
        || path == "/deployment/reconcile"
        || path == "/shell/terminal"
        || path == "/shell/restart"
    {
        return false;
    }
    if matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) {
        return true;
    }
    !(path == "/settings"
        || path == "/env"
        || path.starts_with("/env/")
        || path == "/profiles"
        || path.starts_with("/profiles/")
        || path == "/agents/custom"
        || path.starts_with("/agents/custom/")
        || path == "/mcps/custom"
        || path.starts_with("/mcps/custom/")
        || path == "/watches"
        || path.starts_with("/watches/"))
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
    // An explicit bearer credential is authoritative. Invalid, expired,
    // revoked, or malformed bearer input must not fall through to a valid
    // browser cookie or loopback trust, otherwise revoking a scoped token has
    // no effect on same-host requests.
    if headers.contains_key(header::AUTHORIZATION) {
        let token = bearer_token(headers)?;
        return auth::lookup_token(&st.db, &token).await.ok().flatten();
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
            if !grant_allows(&st, &principal, req.method(), req.uri().path()).await {
                let message =
                    weaver_api::operation_for_request(req.method().as_str(), req.uri().path())
                        .map(|operation| match operation.actor {
                            weaver_api::ActorPolicy::User | weaver_api::ActorPolicy::Admin
                                if matches!(&principal.grant, Grant::Session { .. }) =>
                            {
                                "this operation requires a human operator"
                            }
                            _ => "credential lacks this operation's registered capability or scope",
                        })
                        .unwrap_or("credential grant forbids this route");
                return AppError::new(StatusCode::FORBIDDEN, message).into_response();
            }
            req.extensions_mut().insert(principal);
            next.run(req).await
        }
        // No credential. An operation that declares `actor = Anonymous` is
        // reachable anyway — that declaration is what opens the door, and
        // `authorize()` still runs, against `Grant::Anonymous`. Everything else
        // is refused here as before.
        None => {
            let anonymous_target =
                weaver_api::operation_for_request(req.method().as_str(), req.uri().path())
                    .is_some_and(|operation| operation.actor == weaver_api::ActorPolicy::Anonymous);
            if !anonymous_target {
                return unauthorized("authentication required").into_response();
            }
            req.extensions_mut().insert(Principal::anonymous());
            next.run(req).await
        }
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

/// [`external_base`], falling back to the address we are bound to when the
/// request carries no Host — for building a link to hand out (a webhook reply, a
/// PR back-link, `loom session url`, an artifact URL), where there is no "no
/// origin" answer and the bound address is the best guess available. A wildcard
/// host is mapped to loopback (see [`dialable_host`]) so the link resolves.
pub(crate) async fn public_base(st: &AppState, headers: &HeaderMap) -> String {
    let base = external_base(st, headers)
        .await
        .unwrap_or_else(|| format!("http://{}", st.addr));
    dialable_host(&base)
}

/// Map a wildcard host (`0.0.0.0` / `[::]`, "every interface" — not a dialable
/// address) in a base URL to loopback, so a link we hand out actually resolves.
/// A configured `auth.base_url` or a real browser's Host never carries a
/// wildcard, so this only rewrites the degenerate case: a wildcard-bound server
/// with no public origin declared, asked for a link by a caller (the `weaver`
/// CLI) that dialed that same wildcard address.
fn dialable_host(base: &str) -> String {
    base.replace("://0.0.0.0", "://127.0.0.1")
        .replace("://[::]", "://127.0.0.1")
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

/// `POST /api/auth/login` — username/password. Sets the session cookie.
pub(super) async fn auth_login(
    State(st): State<AppState>,
    Json(body): Json<LoginReq>,
) -> ApiResult<Response> {
    let principal = auth::verify_login(&st.db, body.username.trim(), &body.password)
        .await?
        .ok_or_else(|| unauthorized("invalid username or password"))?;
    let cookie = auth::create_session(&st.db, &principal.username).await?;
    tracing::info!(username = %principal.username, method = "password", "signed in");
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
        if let Ok(Some(principal)) = auth::lookup_session(&st.db, &cookie).await {
            tracing::info!(username = %principal.username, "signed out");
        }
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
    let gh = auth::fetch_github_user(&token).await?;
    let Some(user) = auth::user_by_github(&st.db, &gh.login).await? else {
        // Authenticated with GitHub, but not on the allowlist.
        return Ok(login_error_redirect("not-approved"));
    };
    // Record the profile for commit attribution (best-effort — a failure here
    // must not block a valid sign-in).
    if let Err(e) = auth::update_github_profile(&st.db, &gh.login, gh.id, gh.name.as_deref()).await
    {
        tracing::warn!(login = %gh.login, "failed to record GitHub profile: {e}");
    }
    let cookie = auth::create_session(&st.db, &user.username).await?;
    tracing::info!(username = %user.username, method = "github", "signed in");
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
pub(super) async fn list_tokens(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<Vec<TokenView>>> {
    let tokens = auth::list_tokens(&st.db, &principal.username).await?;
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
    if body
        .expires_in_days
        .is_some_and(|days| days > 0 && weaver_core::db::iso_in_days(days).is_none())
    {
        return Err(AppError::bad_request(
            "token expiry is outside the supported range",
        ));
    }
    let (token, info) =
        auth::create_token(&st.db, &principal.username, name, body.expires_in_days).await?;
    tracing::info!(username = %principal.username, id = %info.id, name = %info.name, "api token created");
    Ok(Json(CreatedTokenView {
        token,
        info: token_view(info),
    }))
}

/// `DELETE /api/auth/tokens/{id}` — revoke a token.
pub(super) async fn revoke_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if auth::revoke_token(&st.db, &principal.username, &id).await? {
        tracing::info!(username = %principal.username, id = %id, "api token revoked");
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

// -- Per-user GitHub token ---------------------------------------------------
// Loom stores the caller's fine-grained PAT and injects it only into ordinary
// interactive sessions they launch. It is the sole direct credential source;
// sessions without one use their profile-approved GitHub App credential.
// Self-service and write-only: no endpoint ever returns the token value.

#[derive(Debug, Deserialize)]
pub(super) struct SetGithubTokenReq {
    token: String,
}

pub(super) async fn get_github_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<user_token::TokenStatus>> {
    Ok(Json(user_token::status(&st.db, &principal.username).await?))
}

pub(super) async fn set_github_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<SetGithubTokenReq>,
) -> ApiResult<Json<user_token::TokenStatus>> {
    let token = body.token.trim();
    if token.is_empty() {
        return Err(AppError::bad_request("a token is required"));
    }
    user_token::set(&st.db, &principal.username, token).await?;
    Ok(Json(user_token::status(&st.db, &principal.username).await?))
}

pub(super) async fn delete_github_token(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<StatusCode> {
    user_token::remove(&st.db, &principal.username).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn user_view(u: auth::User) -> UserView {
    let has_password = u.has_password();
    UserView {
        username: u.username,
        github_login: u.github_login,
        has_password,
        role: user_role_view(u.role),
        created_at: u.created_at,
    }
}

fn user_role_view(role: auth::UserRole) -> UserRole {
    match role {
        auth::UserRole::Admin => UserRole::Admin,
        auth::UserRole::User => UserRole::User,
    }
}

fn user_role_input(role: UserRole) -> auth::UserRole {
    match role {
        UserRole::Admin => auth::UserRole::Admin,
        UserRole::User => auth::UserRole::User,
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
    auth::add_user(
        &st.db,
        username,
        github,
        password,
        user_role_input(body.role),
    )
    .await
    .map_err(|e| AppError::bad_request(format!("could not add user: {e}")))?;
    tracing::info!(username, "operator added");
    let user = auth::get_user(&st.db, username)
        .await?
        .ok_or_else(|| AppError::not_found("user"))?;
    Ok(Json(user_view(user)))
}

/// `PUT /api/auth/users/{username}/role` — update one human role. Existing
/// cookies and personal tokens observe the change on their next request.
pub(super) async fn set_user_role(
    State(st): State<AppState>,
    Path(username): Path<String>,
    Json(body): Json<SetUserRoleReq>,
) -> ApiResult<Json<UserView>> {
    auth::set_user_role(&st.db, &username, user_role_input(body.role))
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    tracing::info!(username, role = ?body.role, "user role updated");
    let user = auth::get_user(&st.db, &username)
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
        Ok(true) => {
            tracing::info!(username = %username, "operator removed");
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err(AppError::not_found("user")),
        Err(e) => Err(AppError::bad_request(e.to_string())),
    }
}

// -- GitHub App / sign-in config ---------------------------------------------
// One GitHub App backs loom: its OAuth client powers sign-in (`configured` /
// `client_id`), and the same App's id + private key power the `@loom` trigger
// (`app_configured` / `app_id` / `app_slug`).

async fn github_config_view(st: &AppState) -> ApiResult<GithubConfigView> {
    // Both the OAuth client id and the App identity are resolved env-or-settings
    // (via `auth`/`github_app`), so an env-configured deploy reports its live
    // values instead of blanks read from an empty settings table.
    let app_id = crate::github_app::app_id(&st.db)
        .await
        .map(|id| id.to_string())
        .unwrap_or_default();
    Ok(GithubConfigView {
        configured: auth::github_oauth(&st.db).await.is_some(),
        client_id: auth::oauth_client_id(&st.db).await,
        callback_path: GITHUB_CALLBACK_PATH.to_string(),
        app_configured: crate::github_app::is_configured(&st.db).await,
        app_id,
        app_slug: crate::github_app::app_slug(&st.db)
            .await
            .unwrap_or_default(),
    })
}

/// `GET /api/auth/github/config` — the GitHub sign-in setup (secret withheld).
pub(super) async fn get_github_config(
    State(st): State<AppState>,
) -> ApiResult<Json<GithubConfigView>> {
    Ok(Json(github_config_view(&st).await?))
}

/// `PUT /api/auth/github/config` — set the sign-in OAuth client id (and,
/// optionally, its secret).
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
    let secret_provided = body.client_secret.is_some();
    if let Some(secret) = body.client_secret {
        let secret = secret.trim().to_string();
        changes.push((
            auth::GH_CLIENT_SECRET_KEY.to_string(),
            (!secret.is_empty()).then_some(secret),
        ));
    }
    config::apply(&st.db, &changes).await?;
    tracing::info!(secret_provided, "github oauth config updated");
    Ok(Json(github_config_view(&st).await?))
}

// ===========================================================================
// Operation registry bindings
//
// `auth.login`, `auth.logout`, and `auth.federate` are declared in
// `weaver_api::operations::auth` but are deliberately NOT bound here — see the
// doc comment on each below. Every other operation in the bundle is bound.
// ===========================================================================

/// `auth.me` — who the caller is. Unlike the legacy `GET /api/auth/me` above
/// (which this leaves untouched), this operation is only ever reached once
/// `require_auth` has already resolved a [`Principal`] — the registry's
/// `actor = User` bars an anonymous caller before the handler runs — so it
/// always answers `authenticated: true`. The "is anyone logged in yet" check
/// the SPA needs before that point stays served by the legacy route.
/// `auth.me` — who the caller is + which sign-in methods to offer.
///
/// Declared `actor = Anonymous`, so an unauthenticated caller arrives here with
/// a synthesized anonymous principal rather than a 401, and gets
/// `authenticated: false`. That is what the login screen reads. This replaces
/// the hand-written public `GET /api/auth/me`, which served the same two cases
/// from a second handler on a second router.
async fn me_op(context: OperationContext, _input: me::Input) -> ApiResult<MeView> {
    let methods = auth_methods(&context.state).await;
    let p = context.principal;
    if matches!(p.grant, Grant::Anonymous) {
        return Ok(MeView {
            authenticated: false,
            role: None,
            username: None,
            github_login: None,
            via: None,
            methods,
        });
    }
    Ok(MeView {
        authenticated: true,
        role: p.user_role().map(user_role_view),
        username: Some(p.username),
        github_login: p.github_login,
        via: Some(p.via.as_str().to_string()),
        methods,
    })
}

/// `auth.automation_token` — mint a short-lived automation credential for
/// another subject. `actor = Admin` on the descriptor now does what the old
/// handler's `principal.is_admin()` check did by hand; that inline check is
/// deleted rather than ported.
async fn automation_token_op(
    context: OperationContext,
    input: automation_token::Input,
) -> ApiResult<AutomationTokenView> {
    crate::automation::mint(
        &context.state.db,
        &input.subject,
        input.profiles,
        input.ttl_secs,
        None,
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))
}

/// `auth.set_password` — set/change the caller's own password. There is no
/// `username` in the input at all: the operation can only ever touch
/// `context.principal`'s own account, which is the ownership guarantee the
/// old handler enforced by construction (it never took a `username` either).
async fn set_password_op(
    context: OperationContext,
    input: set_password::Input,
) -> ApiResult<UserView> {
    if input.new_password.len() < 8 {
        return Err(AppError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    auth::set_password(
        &context.state.db,
        &context.principal.username,
        Some(&input.new_password),
    )
    .await?;
    let user = auth::get_user(&context.state.db, &context.principal.username)
        .await?
        .ok_or_else(|| AppError::not_found("user"))?;
    Ok(user_view(user))
}

/// `auth.tokens.list` — the caller's own personal tokens (metadata only).
/// Scoped to `context.principal.username` by construction, same as before.
async fn list_tokens_op(
    context: OperationContext,
    _input: tokens::list::Input,
) -> ApiResult<Vec<TokenView>> {
    let tokens = auth::list_tokens(&context.state.db, &context.principal.username).await?;
    Ok(tokens.into_iter().map(token_view).collect())
}

/// `auth.tokens.create` — mint a personal token owned by the caller. The
/// plaintext is returned exactly once, here; nothing else ever hands it back.
async fn create_token_op(
    context: OperationContext,
    input: tokens::create::Input,
) -> ApiResult<CreatedTokenView> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("a token name is required"));
    }
    if input
        .expires_in_days
        .is_some_and(|days| days > 0 && weaver_core::db::iso_in_days(days).is_none())
    {
        return Err(AppError::bad_request(
            "token expiry is outside the supported range",
        ));
    }
    let (token, info) = auth::create_token(
        &context.state.db,
        &context.principal.username,
        name,
        input.expires_in_days,
    )
    .await?;
    tracing::info!(username = %context.principal.username, id = %info.id, name = %info.name, "api token created");
    Ok(CreatedTokenView {
        token,
        info: token_view(info),
    })
}

/// `auth.tokens.revoke` — revoke one of the caller's own tokens.
/// `auth::revoke_token`'s query is `WHERE id = ? AND username = ?`: a caller
/// cannot name another user's token id and revoke it, because no row matches.
/// That ownership check lives in the query, not a separate `if`, and is
/// preserved unchanged here (see point 4 in the report).
async fn revoke_token_op(
    context: OperationContext,
    input: tokens::revoke::Input,
) -> ApiResult<RevokeTokenResult> {
    if auth::revoke_token(&context.state.db, &context.principal.username, &input.id).await? {
        tracing::info!(username = %context.principal.username, id = %input.id, "api token revoked");
        Ok(RevokeTokenResult {
            revoked: true,
            id: input.id,
        })
    } else {
        Err(AppError::not_found("token"))
    }
}

/// `auth.federations.list` — the registered workload-identity mappings.
async fn list_federations_op(
    context: OperationContext,
    _input: federations::list::Input,
) -> ApiResult<Vec<FederationView>> {
    Ok(crate::automation::federation_list(&context.state.db).await?)
}

/// Best-effort name when the caller omits one, per the operation's own doc
/// comment: "Omitted legacy calls derive one from the identity fields below."
/// `crate::automation::federation_add` upserts on `name` and requires it to
/// pass `crate::profile::validate_name` (starts with a letter; then letters,
/// digits, `-`, `_`; at most 64 bytes), so the chosen identity field is
/// slugified into that shape rather than passed through raw.
fn derive_federation_name(input: &federations::create::Input) -> String {
    let seed = match input.provider.trim().to_ascii_lowercase().as_str() {
        "google" => input
            .service_account
            .as_deref()
            .or(input.subject.as_deref()),
        _ => input
            .repository_id
            .as_deref()
            .or(input.workflow_ref.as_deref()),
    }
    .unwrap_or(input.service_tag.as_str());
    let slug: String = seed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let mut name = format!("{}-{slug}", input.provider.trim().to_ascii_lowercase());
    name.truncate(64);
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        name = format!("f-{name}");
        name.truncate(64);
    }
    name
}

/// `auth.federations.create` — register or reconcile a workload-identity
/// mapping.
async fn create_federation_op(
    context: OperationContext,
    input: federations::create::Input,
) -> ApiResult<FederationView> {
    let name = match input.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => derive_federation_name(&input),
    };
    let req = FederationReq {
        name,
        provider: input.provider,
        issuer: input.issuer,
        audience: input.audience,
        subject: input.subject,
        service_account: input.service_account,
        service_tag: input.service_tag,
        repository_id: input.repository_id,
        workflow_ref: input.workflow_ref,
        event_name: input.event_name,
        ref_pattern: input.ref_pattern,
        profiles: input.profiles,
    };
    crate::automation::federation_add(&context.state.db, &req)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))
}

/// `auth.federations.remove` — remove a workload-identity mapping.
async fn remove_federation_op(
    context: OperationContext,
    input: federations::remove::Input,
) -> ApiResult<RemoveFederationResult> {
    if crate::automation::federation_remove(&context.state.db, &input.id).await? {
        Ok(RemoveFederationResult {
            removed: true,
            id: input.id,
        })
    } else {
        Err(AppError::not_found("federation mapping"))
    }
}

/// `auth.users.list` — the approved-operator allowlist.
async fn list_users_op(
    context: OperationContext,
    _input: users::list::Input,
) -> ApiResult<Vec<UserView>> {
    let users = auth::list_users(&context.state.db).await?;
    Ok(users.into_iter().map(user_view).collect())
}

/// `auth.users.create` — approve a new operator.
async fn add_user_op(
    context: OperationContext,
    input: users::create::Input,
) -> ApiResult<UserView> {
    let username = input.username.trim();
    if username.is_empty() {
        return Err(AppError::bad_request("a username is required"));
    }
    let github = input
        .github_login
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = input.password.as_deref().filter(|s| !s.is_empty());
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
    auth::add_user(
        &context.state.db,
        username,
        github,
        password,
        user_role_input(input.role),
    )
    .await
    .map_err(|e| AppError::bad_request(format!("could not add user: {e}")))?;
    tracing::info!(username, "operator added");
    let user = auth::get_user(&context.state.db, username)
        .await?
        .ok_or_else(|| AppError::not_found("user"))?;
    Ok(user_view(user))
}

/// `auth.users.set_role` — change an operator's role. Admin-only via
/// `actor = Admin`; any admin may change any user's role, including their
/// own, matching the old handler (which took no principal at all). The
/// "don't demote the last administrator" business rule lives inside
/// `auth::set_user_role` and is preserved unchanged.
async fn set_user_role_op(
    context: OperationContext,
    input: users::set_role::Input,
) -> ApiResult<UserView> {
    auth::set_user_role(
        &context.state.db,
        &input.username,
        user_role_input(input.role),
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    tracing::info!(username = %input.username, role = ?input.role, "user role updated");
    let user = auth::get_user(&context.state.db, &input.username)
        .await?
        .ok_or_else(|| AppError::not_found("user"))?;
    Ok(user_view(user))
}

/// `auth.users.remove` — remove an approved operator. The "you cannot remove
/// yourself" check is business state (self-removal would strand the acting
/// admin, or worse, be used to strand others), not an authority check
/// `authorize()` could express, so it is kept exactly as before (point 4).
async fn remove_user_op(
    context: OperationContext,
    input: users::remove::Input,
) -> ApiResult<RemoveUserResult> {
    if input.username == context.principal.username {
        return Err(AppError::bad_request("you cannot remove yourself"));
    }
    match auth::remove_user(&context.state.db, &input.username).await {
        Ok(true) => {
            tracing::info!(username = %input.username, "operator removed");
            Ok(RemoveUserResult {
                removed: true,
                username: input.username,
            })
        }
        Ok(false) => Err(AppError::not_found("user")),
        Err(e) => Err(AppError::bad_request(e.to_string())),
    }
}

fn token_status_view(status: user_token::TokenStatus) -> GithubTokenStatusView {
    GithubTokenStatusView {
        set: status.set,
        updated_at: status.updated_at,
    }
}

/// `auth.github_token.get` — whether the caller has a personal GitHub token
/// on file. Scoped to `context.principal.username`; the value itself is
/// never returned by this or any other operation.
async fn get_github_token_op(
    context: OperationContext,
    _input: github_token::get::Input,
) -> ApiResult<GithubTokenStatusView> {
    let status = user_token::status(&context.state.db, &context.principal.username).await?;
    Ok(token_status_view(status))
}

/// `auth.github_token.set` — set the caller's own personal GitHub token.
async fn set_github_token_op(
    context: OperationContext,
    input: github_token::set::Input,
) -> ApiResult<GithubTokenStatusView> {
    let token = input.token.trim();
    if token.is_empty() {
        return Err(AppError::bad_request("a token is required"));
    }
    user_token::set(&context.state.db, &context.principal.username, token).await?;
    let status = user_token::status(&context.state.db, &context.principal.username).await?;
    Ok(token_status_view(status))
}

/// `auth.github_token.remove` — clear the caller's own personal GitHub token.
async fn remove_github_token_op(
    context: OperationContext,
    _input: github_token::remove::Input,
) -> ApiResult<GithubTokenStatusView> {
    user_token::remove(&context.state.db, &context.principal.username).await?;
    let status = user_token::status(&context.state.db, &context.principal.username).await?;
    Ok(token_status_view(status))
}

/// `auth.github_config.get` — the GitHub sign-in / App setup (secret
/// withheld). Reuses the same `github_config_view` the legacy handler above
/// builds its response from.
async fn get_github_config_op(
    context: OperationContext,
    _input: github_config::get::Input,
) -> ApiResult<GithubConfigView> {
    github_config_view(&context.state).await
}

/// `auth.github_config.set` — set the GitHub sign-in OAuth client id (and,
/// optionally, its secret).
async fn set_github_config_op(
    context: OperationContext,
    input: github_config::set::Input,
) -> ApiResult<GithubConfigView> {
    let mut changes: Vec<config::Change> = vec![(
        auth::GH_CLIENT_ID_KEY.to_string(),
        Some(input.client_id.trim().to_string()),
    )];
    let secret_provided = input.client_secret.is_some();
    if let Some(secret) = input.client_secret {
        let secret = secret.trim().to_string();
        changes.push((
            auth::GH_CLIENT_SECRET_KEY.to_string(),
            (!secret.is_empty()).then_some(secret),
        ));
    }
    config::apply(&context.state.db, &changes).await?;
    tracing::info!(secret_provided, "github oauth config updated");
    github_config_view(&context.state).await
}

/// The `auth` bundle's operation bindings. `auth.login`, `auth.logout`, and
/// `auth.federate` are intentionally absent — see the doc comments on
/// [`me_op`] and the module-level report for why they cannot go through this
/// path without either bypassing authentication or inventing new transport
/// plumbing this registry doesn't have.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<me::Me, _, _>(me_op),
        register::<automation_token::AutomationToken, _, _>(automation_token_op),
        register::<set_password::SetPassword, _, _>(set_password_op),
        register::<tokens::list::List, _, _>(list_tokens_op),
        register::<tokens::create::Create, _, _>(create_token_op),
        register::<tokens::revoke::Revoke, _, _>(revoke_token_op),
        register::<federations::list::List, _, _>(list_federations_op),
        register::<federations::create::Create, _, _>(create_federation_op),
        register::<federations::remove::Remove, _, _>(remove_federation_op),
        register::<users::list::List, _, _>(list_users_op),
        register::<users::create::Create, _, _>(add_user_op),
        register::<users::set_role::SetRole, _, _>(set_user_role_op),
        register::<users::remove::Remove, _, _>(remove_user_op),
        register::<github_token::get::Get, _, _>(get_github_token_op),
        register::<github_token::set::Set, _, _>(set_github_token_op),
        register::<github_token::remove::Remove, _, _>(remove_github_token_op),
        register::<github_config::get::Get, _, _>(get_github_config_op),
        register::<github_config::set::Set, _, _>(set_github_config_op),
    ]
}

#[cfg(test)]
mod tests {
    use super::{dialable_host, operation_grant_allows};
    use crate::auth::{AuthVia, Grant, Principal};

    #[test]
    fn wildcard_hosts_map_to_loopback() {
        // A wildcard bind is "every interface", not a dialable address — the
        // link we hand out must point somewhere a browser can actually open.
        assert_eq!(
            dialable_host("http://0.0.0.0:7878"),
            "http://127.0.0.1:7878"
        );
        assert_eq!(dialable_host("http://[::]:7878"), "http://127.0.0.1:7878");
    }

    #[test]
    fn real_origins_pass_through_untouched() {
        // A configured `auth.base_url` or a real browser's Host never carries a
        // wildcard, so the common cases are left exactly as-is.
        assert_eq!(
            dialable_host("https://loom.example.com"),
            "https://loom.example.com"
        );
        assert_eq!(
            dialable_host("http://127.0.0.1:7878"),
            "http://127.0.0.1:7878"
        );
        // `0.0.0.0` only elsewhere in the string (not as the host) is left alone.
        assert_eq!(
            dialable_host("https://host/p/0.0.0.0"),
            "https://host/p/0.0.0.0"
        );
    }

    #[test]
    fn registered_capabilities_are_enforced_before_adapter_routing() {
        let principal = Principal {
            username: "agent".to_string(),
            github_login: None,
            via: AuthVia::Token,
            grant: Grant::Session {
                session_id: "s".to_string(),
                branch_id: "b".to_string(),
                capabilities: Some(vec!["loom/artifacts/read@v1".to_string()]),
            },
            automation_context: None,
        };
        assert!(operation_grant_allows(
            &principal,
            weaver_api::operation("artifacts.get").unwrap()
        ));
        assert!(!operation_grant_allows(
            &principal,
            weaver_api::operation("artifacts.write").unwrap()
        ));
        assert!(!operation_grant_allows(
            &principal,
            weaver_api::operation("permissions.requests.approve").unwrap()
        ));
    }
}
