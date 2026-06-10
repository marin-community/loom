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
pub fn default() -> Client {
    Client::new(crate::endpoint::base_url())
}
