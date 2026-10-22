//! Loom's remote MCP endpoint: Streamable HTTP at `POST /mcp`.
//!
//! This is a transport, like the IDE proxy, not an operation. It lets a
//! person's assistant — a claude.ai custom connector, say — launch and follow
//! sessions with the person's own credential. The tools are
//! `loom_agent::mcp::connector`'s reviewed list, each a registered operation;
//! a call runs in-process through the same dispatcher and `authorize()` as
//! REST, so the endpoint cannot widen what the credential may do.
//!
//! The server is stateless: no `Mcp-Session-Id`, one JSON response per
//! request, and no server-to-client stream, so `GET` gets the `405` the
//! protocol allows. Only a bearer token authenticates — never the login
//! cookie — so a browser page cannot ride a person's sign-in into a tool call,
//! and the `Origin` check the protocol asks for against DNS rebinding has no
//! ambient credential to protect. A missing or invalid token gets a `401`
//! naming the OAuth metadata in [`super::oauth`], which is how a client
//! discovers where to sign the person in.

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::auth::{self, Principal};
use crate::config;
use crate::mcp::{connector, handle_message, ServerInfo, PROTOCOL_VERSIONS};
use crate::AppState;

use super::auth::{bearer_token, public_base};
use super::oauth::{connector_enabled, resource_metadata_url};

const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": code, "message": message }
        })),
    )
        .into_response()
}

/// `401` with the RFC 9728 pointer a client follows to find the
/// authorization server.
fn unauthorized(base: &str, presented: bool) -> Response {
    let mut challenge = format!(
        "Bearer resource_metadata=\"{}\"",
        resource_metadata_url(base)
    );
    if presented {
        challenge.push_str(", error=\"invalid_token\"");
    }
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "a Loom bearer token is required" })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// The person behind the bearer token. Session and automation credentials
/// have their own MCP server and their own operations; this door is for
/// people.
async fn authenticate(st: &AppState, headers: &HeaderMap) -> Result<Principal, Box<Response>> {
    let base = public_base(st, headers).await;
    let Some(token) = bearer_token(headers) else {
        return Err(Box::new(unauthorized(
            &base,
            headers.contains_key(header::AUTHORIZATION),
        )));
    };
    let principal = match auth::lookup_token(&st.db, &token).await {
        Ok(Some(principal)) => principal,
        Ok(None) => return Err(Box::new(unauthorized(&base, true))),
        Err(error) => {
            tracing::error!(%error, "MCP bearer lookup failed");
            return Err(Box::new(rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "credential lookup failed",
            )));
        }
    };
    if !principal.is_human() {
        return Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "the remote MCP endpoint serves people's credentials" })),
            )
                .into_response(),
        ));
    }
    Ok(principal)
}

/// `POST /mcp` — one JSON-RPC message in, at most one response out.
pub(super) async fn post_mcp(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !connector_enabled(&st).await {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    }
    let principal = match authenticate(&st, &headers).await {
        Ok(principal) => principal,
        Err(response) => return *response,
    };
    if let Some(version) = headers.get(PROTOCOL_VERSION_HEADER) {
        let supported = version
            .to_str()
            .is_ok_and(|version| PROTOCOL_VERSIONS.contains(&version));
        if !supported {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                -32600,
                "unsupported MCP-Protocol-Version",
            );
        }
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => return rpc_error(StatusCode::BAD_REQUEST, -32700, "parse error"),
    };
    if !message.is_object() {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "send one JSON-RPC message per request; batches are not supported",
        );
    }
    // Notifications and responses to server requests need no reply.
    if message.get("method").is_none() || message.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let server = ServerInfo {
        name: "loom",
        instructions: Some(connector::INSTRUCTIONS),
    };
    let response = handle_message(&message, &server, connector::tools, |tool, arguments| {
        call_tool(st.clone(), principal.clone(), tool, arguments)
    })
    .await;
    match response {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Run one connector tool as `principal`.
async fn call_tool(
    st: AppState,
    principal: Principal,
    tool: String,
    mut arguments: Value,
) -> anyhow::Result<Value> {
    let export =
        connector::lookup(&tool).ok_or_else(|| anyhow::anyhow!("unknown tool '{tool}'"))?;
    if let Value::Object(object) = &mut arguments {
        for (field, key) in export.setting_defaults() {
            if object.contains_key(*field) {
                continue;
            }
            let value = match config::get(&st.db, key).await {
                Some(value) => value,
                None => config::spec(key)
                    .map(|spec| spec.default.to_string())
                    .unwrap_or_default(),
            };
            if !value.trim().is_empty() {
                object.insert(field.to_string(), Value::String(value.trim().to_string()));
            }
        }
    }
    let input = export.prepare(arguments)?;
    let username = principal.username.clone();
    match super::operations::invoke(st, principal, export.operation.id, input).await {
        Ok(output) => export.respond(output),
        Err(error) => {
            tracing::warn!(
                tool = %tool,
                operation = export.operation.id,
                username = %username,
                message = %error.message(),
                "MCP tool call refused"
            );
            Err(anyhow::anyhow!(error.message().to_string()))
        }
    }
}
