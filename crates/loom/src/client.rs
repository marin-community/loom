//! The loom HTTP client every CLI subcommand (except `server run`) uses.
//!
//! The client itself — typed methods and untyped JSON transport — lives in
//! [`weaver_api::Client`], shared with the Python binding and any other
//! out-of-process consumer. This module re-exports it and supplies the default
//! base URL from [`crate::endpoint`], so loom's callers get a client pointed at
//! the running daemon with no configuration.

pub use weaver_api::Client;

/// A client pointed at the running daemon — the base URL resolved from
/// `$WEAVER_API`, the recorded server address, then the default
/// (see [`crate::endpoint::base_url`]).
///
/// The bearer token is resolved from `$LOOM_TOKEN`, falling back to the
/// machine-local token file ([`crate::auth::local_token_path`]) so a same-host
/// CLI works even when loopback trust is off and the user set no env var. A
/// remote CLI (talking to a server on another host) has no local file, so it
/// relies on `$LOOM_TOKEN` being set — as it must, since loopback trust can't
/// apply across the network.
pub fn default() -> Client {
    Client::new(crate::endpoint::base_url()).with_token(resolve_token())
}

/// The bearer token a local CLI should present: `$LOOM_TOKEN`, else the
/// persisted machine-local token, else none.
fn resolve_token() -> Option<String> {
    if let Ok(t) = std::env::var("LOOM_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    crate::agent::read_local_token()
}
