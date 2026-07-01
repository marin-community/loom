//! Resolving the weaver server's address — the single source of truth shared by
//! `weaver server run` (which binds it) and every other subcommand (which
//! connects to it).
//!
//! `$WEAVER_API` configures both sides at once: set it and the server binds
//! there and clients look there. With nothing set, the server records the
//! address it bound in `server.json` and clients read it back, so the common
//! case needs no configuration at all.

use crate::server;

/// The `host:port` used when nothing else is configured.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// Reduce a `$WEAVER_API` value — a URL (`http://host:port`) or a bare
/// `host:port` — to a plain `host:port` socket string.
fn socket_part(api: &str) -> String {
    api.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

/// `$WEAVER_API` normalized to `host:port`, if set to a non-empty value.
fn env_addr() -> Option<String> {
    std::env::var("WEAVER_API")
        .ok()
        .map(|v| socket_part(&v))
        .filter(|s| !s.is_empty())
}

/// The `host:port` the server should bind: an explicit `--addr` wins, then
/// `$WEAVER_API`, then the default.
///
/// The running server's `server.json` is deliberately ignored here — the
/// server *creates* the endpoint, it does not discover an existing one.
pub fn bind_addr(override_addr: Option<&str>) -> String {
    let explicit = override_addr
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let (source, addr) = if let Some(a) = explicit {
        ("override", a)
    } else if let Some(a) = env_addr() {
        ("WEAVER_API", a)
    } else {
        ("default", DEFAULT_ADDR.to_string())
    };
    tracing::debug!(addr = %addr, source, "resolved bind address");
    addr
}

/// The `host:port` a client should connect to: `$WEAVER_API`, then the running
/// server's recorded address, then the default.
pub fn client_addr() -> String {
    env_addr()
        .or_else(|| server::read_state().map(|s| s.addr))
        .unwrap_or_else(|| DEFAULT_ADDR.to_string())
}

/// The scheme `$WEAVER_API` carries explicitly (`http` or `https`), or `None`
/// when it's unset or a bare `host:port`.
fn env_scheme() -> Option<&'static str> {
    let api = std::env::var("WEAVER_API").ok()?;
    let api = api.trim();
    if api.starts_with("https://") {
        Some("https")
    } else if api.starts_with("http://") {
        Some("http")
    } else {
        None
    }
}

/// The base URL a client should use. `$WEAVER_API`'s scheme is preserved when
/// it specifies one explicitly, so a remote `https://` deployment isn't
/// silently downgraded to plaintext; otherwise `http`, since the recorded or
/// default address is always loopback-bound.
pub fn base_url() -> String {
    let scheme = env_scheme().unwrap_or("http");
    format!("{scheme}://{}", client_addr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_part_strips_scheme_and_trailing_slash() {
        assert_eq!(socket_part("http://127.0.0.1:7878"), "127.0.0.1:7878");
        assert_eq!(socket_part("https://host:9000/"), "host:9000");
        assert_eq!(socket_part("127.0.0.1:7878"), "127.0.0.1:7878");
        assert_eq!(socket_part("  http://h:1  "), "h:1");
    }

    #[test]
    fn bind_addr_prefers_an_explicit_override() {
        assert_eq!(bind_addr(Some("0.0.0.0:9999")), "0.0.0.0:9999");
        // A blank override falls through to the same chain as `None`.
        assert_eq!(bind_addr(Some("   ")), bind_addr(None));
    }
}
