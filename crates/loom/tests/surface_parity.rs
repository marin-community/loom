//! The parallel-surface ledger.
//!
//! Every `.route(...)` the server mounts by hand must be accounted for: either
//! it is a transport that the operation DSL genuinely does not model (a stream,
//! a websocket, a proxy, an unauthenticated probe), or it is a legacy route that
//! an operation has superseded and that has not been deleted yet.
//!
//! The previous registry had no such ledger, which is how it ended up declaring
//! operations whose routes the server had stopped serving. Here the list is
//! written down, so "we still have two surfaces" is a number that has to go to
//! zero rather than an impression.

use std::collections::BTreeSet;

/// Routes that are deliberately NOT operations, with the reason.
///
/// Adding a line here is a decision that the operation DSL cannot express this
/// endpoint. It is not a place to park work.
const TRANSPORT_ROUTES: &[(&str, &str)] = &[
    // Server-sent event streams. `Operands` projects to a JSON body; a GET
    // stream needs its arguments in the URL, and these routes carry a path
    // parameter, so their real route cannot equal a derived operation path.
    ("/events", "SSE: global event mux"),
    ("/logs/stream", "SSE: log tail"),
    ("/session-layout/events", "SSE: layout changes"),
    ("/sessions/{id}/events", "SSE: one session's events"),
    ("/sessions/{id}/chat/stream", "SSE: assistant token stream"),
    // Websockets.
    ("/sessions/{id}/terminal", "websocket: session terminal"),
    ("/shell/terminal", "websocket: standalone shell"),
    // Reverse proxy to the embedded editor.
    ("/sessions/{id}/ide", "proxy: code-server"),
    ("/sessions/{id}/ide/", "proxy: code-server"),
    // Unauthenticated infrastructure probes: no principal, no JSON contract.
    ("/health", "probe"),
    ("/health/live", "probe"),
    ("/health/ready", "probe"),
    ("/ready", "probe"),
    ("/metrics", "probe: prometheus text format"),
    // Authenticated by HMAC over the raw body, not by a Loom principal.
    ("/github/webhook", "inbound webhook"),
    // Browser OAuth redirects: 303 + Set-Cookie, never a JSON body.
    ("/auth/github/login", "oauth redirect"),
    ("/auth/github/callback", "oauth redirect"),
    // The three `io = Session` operations. They ARE registered and appear in the
    // surface; they are mounted here because their response must carry a
    // Set-Cookie, which the generic dispatcher cannot emit.
    ("/auth/login", "io = Session"),
    ("/auth/logout", "io = Session"),
    ("/auth/federate", "io = Session"),
    // Registry discovery. These describe operations; making them operations
    // would be circular.
    ("/meta", "discovery"),
    ("/operations", "discovery"),
    ("/operations/{id}", "discovery"),
    ("/openapi.json", "discovery"),
];

fn mounted_routes() -> BTreeSet<String> {
    let source = include_str!("../src/web/mod.rs");
    let mut routes = BTreeSet::new();
    for line in source.lines() {
        let Some(rest) = line.split_once(".route(\"") else {
            continue;
        };
        if let Some((path, _)) = rest.1.split_once('"') {
            routes.insert(path.to_string());
        }
    }
    routes
}

fn operation_routes() -> BTreeSet<String> {
    weaver_api::operations::operations()
        .map(|operation| {
            operation
                .path()
                .strip_prefix("/api")
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every hand-mounted route is either an operation's own route or a declared
/// transport. Anything else is a surface the registry does not know about.
#[test]
fn no_route_is_unaccounted_for() {
    let transport: BTreeSet<&str> = TRANSPORT_ROUTES.iter().map(|(path, _)| *path).collect();
    let operations = operation_routes();

    let unaccounted: Vec<String> = mounted_routes()
        .into_iter()
        .filter(|route| !transport.contains(route.as_str()))
        .filter(|route| !operations.contains(route))
        .collect();

    assert!(
        unaccounted.is_empty(),
        "{} hand-mounted routes are neither an operation nor a declared transport.\n\
         Each is either a legacy route an operation has superseded (delete it) or an\n\
         API with no operation yet (register it):\n{}",
        unaccounted.len(),
        unaccounted
            .iter()
            .map(|route| format!("  {route}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The transport list may not rot: a route listed there must actually exist.
#[test]
fn every_declared_transport_is_mounted() {
    let mounted = mounted_routes();
    let missing: Vec<&str> = TRANSPORT_ROUTES
        .iter()
        .map(|(path, _)| *path)
        .filter(|path| !mounted.contains(*path))
        .collect();
    assert!(
        missing.is_empty(),
        "declared transports that are no longer mounted: {missing:?}"
    );
}
