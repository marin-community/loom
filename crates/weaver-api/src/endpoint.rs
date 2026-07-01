//! Resolving which loom server a client should talk to, and how it should
//! authenticate — the one place this logic lives so both `loom`'s own CLI and
//! the `weaver` CLI (an HTTP-only client of loom, see `crates/weaver`) resolve
//! a server the same way.
//!
//! `$WEAVER_API` configures the target: a bare `host:port` (assumed `http://`)
//! or a full URL — a `https://` scheme is preserved, so a client can be
//! pointed at a remote, TLS-terminated loom deployment. With nothing set, the
//! address the local loom recorded while serving is used; failing that, the
//! loopback default.

use crate::Client;

/// The `host:port` used when nothing else is configured.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// Normalize a `$WEAVER_API` value to a base URL. A value already carrying a
/// scheme (`http://` or `https://`) is used as-is; a bare `host:port` is
/// assumed `http://`.
fn normalize(api: &str) -> String {
    let trimmed = api.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

fn env_base_url() -> Option<String> {
    std::env::var("WEAVER_API")
        .ok()
        .map(|v| normalize(&v))
        .filter(|s| !s.is_empty())
}

/// The `addr` a local loom recorded in `<weaver_home>/loom.json` while
/// serving, read without depending on the `loom` crate — the file is just
/// `{pid, addr, started_at}` (see `loom::server::ServerState`).
fn recorded_addr() -> Option<String> {
    let path = weaver_core::db::weaver_home().join("loom.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("addr")
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The base URL a client should use: `$WEAVER_API`, then the local server's
/// recorded address, then the loopback default. Scheme is preserved
/// throughout — only the fallback paths assume plain `http://` (a locally
/// recorded server is always loopback-bound today).
pub fn base_url() -> String {
    env_base_url()
        .or_else(|| recorded_addr().map(|addr| format!("http://{addr}")))
        .unwrap_or_else(|| format!("http://{DEFAULT_ADDR}"))
}

/// The bearer token a caller should present: `$LOOM_TOKEN`, or `None` when
/// unset — a local, loopback-bound server may still authenticate the request
/// via loopback trust.
pub fn token_from_env() -> Option<String> {
    std::env::var("LOOM_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// A client pointed at the server resolved by [`base_url`], authenticated
/// with [`token_from_env`]. The default construction for any out-of-process
/// caller that doesn't need to override the target explicitly.
pub fn default_client() -> Client {
    Client::new(base_url()).with_token(token_from_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_preserves_an_explicit_scheme() {
        assert_eq!(
            normalize("https://loom.example.com"),
            "https://loom.example.com"
        );
        assert_eq!(normalize("http://host:9000/"), "http://host:9000");
        assert_eq!(normalize("127.0.0.1:7878"), "http://127.0.0.1:7878");
        assert_eq!(normalize("  host:1  "), "http://host:1");
    }
}
