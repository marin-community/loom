//! The loom HTTP client every CLI subcommand (except `server run`) uses.
//!
//! The client itself — typed methods and untyped JSON transport — lives in
//! [`weaver_api::Client`], shared with the Python binding and any other
//! out-of-process consumer. This module re-exports it and supplies the default
//! base URL from the selected [`crate::client_context`], falling back to local
//! daemon discovery from [`crate::endpoint`].

use std::sync::OnceLock;

use anyhow::{bail, Result};

pub use weaver_api::Client;

static CONTEXT_OVERRIDE: OnceLock<String> = OnceLock::new();

pub fn set_context_override(name: Option<&str>) -> Result<()> {
    let Some(name) = name else {
        return Ok(());
    };
    let name = crate::client_context::validate_name(name)?.to_string();
    if let Some(current) = CONTEXT_OVERRIDE.get() {
        if current != &name {
            bail!("Loom context is already set to '{current}'");
        }
        return Ok(());
    }
    CONTEXT_OVERRIDE
        .set(name)
        .map_err(|_| anyhow::anyhow!("could not set Loom context"))
}

/// A client pointed at the selected Loom server.
///
/// `$WEAVER_API` wins over named contexts. Authentication resolves from an
/// explicit `$LOOM_TOKEN`, the selected context's credential, then the
/// machine-local bearer for loopback endpoints only.
pub fn default() -> Result<Client> {
    let context_override = CONTEXT_OVERRIDE.get().map(String::as_str);
    let explicit_endpoint = std::env::var("WEAVER_API")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let context = if context_override.is_some() {
        crate::client_context::resolve(context_override)?
    } else if explicit_endpoint {
        None
    } else {
        crate::client_context::resolve(None)?
    };
    let base = context
        .as_ref()
        .map(|context| context.url.clone())
        .unwrap_or_else(crate::endpoint::base_url);
    let use_environment_token =
        !explicit_endpoint || same_endpoint(&base, &crate::endpoint::base_url());
    let token = resolve_token(
        context.and_then(|context| context.token),
        &base,
        use_environment_token,
    );
    Ok(Client::new(base).with_token(token))
}

/// Resolve a bearer without ever sending the machine-local token to a remote
/// host.
fn resolve_token(
    context_token: Option<String>,
    base: &str,
    use_environment_token: bool,
) -> Option<String> {
    if let Some(t) = std::env::var("LOOM_TOKEN")
        .ok()
        .filter(|_| use_environment_token)
    {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    context_token.or_else(|| {
        is_loopback(base)
            .then(crate::agent::read_local_token)
            .flatten()
    })
}

fn same_endpoint(left: &str, right: &str) -> bool {
    reqwest::Url::parse(left).ok() == reqwest::Url::parse(right).ok()
}

fn is_loopback(base: &str) -> bool {
    reqwest::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
}
