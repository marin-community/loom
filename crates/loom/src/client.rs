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

// The binary parses this once before dispatch. A write-once process value keeps
// the cross-cutting CLI option out of every subcommand signature without
// allowing it to change while an operation is running.
static CONTEXT_OVERRIDE: OnceLock<String> = OnceLock::new();

/// The endpoint selected for this CLI process and why it was selected.
pub struct ClientSelection {
    /// Normalized Loom server URL.
    pub base: String,
    /// Configuration source that selected the endpoint.
    pub source: ClientSelectionSource,
}

/// The source of a selected CLI endpoint.
pub enum ClientSelectionSource {
    /// A named user context selected by a flag, environment, repository, or default.
    Context {
        name: String,
        source: crate::client_context::ContextSource,
    },
    /// The legacy `WEAVER_API` endpoint override.
    Environment,
    /// The recorded or implicit local server.
    Local,
}

struct ResolvedClient {
    selection: ClientSelection,
    context_token: Option<String>,
    use_environment_token: bool,
}

/// Set the global `--context` value once, before CLI dispatch.
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
/// An explicit `--context` wins over `$WEAVER_API`; otherwise the environment
/// endpoint wins over automatic context selection. Authentication normally
/// resolves from `$LOOM_TOKEN`, the selected context's credential, then the
/// machine-local bearer for loopback endpoints. When `--context` selects a
/// different endpoint than `$WEAVER_API`, the paired environment token is not
/// sent to that server.
pub fn default() -> Result<Client> {
    let resolved = resolve_client()?;
    let token = resolve_token(
        resolved.context_token,
        &resolved.selection.base,
        resolved.use_environment_token,
    );
    Ok(Client::new(resolved.selection.base).with_token(token))
}

/// Resolve the endpoint selection without constructing an HTTP client.
pub fn current_selection() -> Result<ClientSelection> {
    Ok(resolve_client()?.selection)
}

fn resolve_client() -> Result<ResolvedClient> {
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
    let environment_base = explicit_endpoint.then(crate::endpoint::base_url);
    let base = context.as_ref().map_or_else(
        || {
            environment_base
                .clone()
                .unwrap_or_else(crate::endpoint::base_url)
        },
        |context| context.url.clone(),
    );
    let use_environment_token = environment_base
        .as_deref()
        .is_none_or(|environment| same_endpoint(&base, environment));
    let (source, context_token) = match context {
        Some(context) => (
            ClientSelectionSource::Context {
                name: context.name,
                source: context.source,
            },
            context.token,
        ),
        None if explicit_endpoint => (ClientSelectionSource::Environment, None),
        None => (ClientSelectionSource::Local, None),
    };
    Ok(ResolvedClient {
        selection: ClientSelection { base, source },
        context_token,
        use_environment_token,
    })
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
