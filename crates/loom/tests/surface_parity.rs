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

/// Legacy routes an operation has superseded, still mounted for the browser.
///
/// Every one of these has a registered operation serving the same data at
/// `POST /api/<id>`; what keeps them alive is `crates/loom/frontend`, which still
/// calls them. This list is a ratchet, not a parking space: a route that is no
/// longer mounted must be deleted from it, and a hand-mounted route that is not
/// here fails [`no_route_is_unaccounted_for`]. The number that has to reach zero
/// is 77.
const SUPERSEDED_ROUTES: &[&str] = &[
    "/agent/oneshot",
    "/agents",
    "/agents/custom",
    "/auth/automation-token",
    "/auth/federations/{id}",
    "/auth/password",
    "/auth/tokens",
    "/auth/tokens/{id}",
    "/auth/users",
    "/auth/users/{username}",
    "/branches",
    "/branches/{id}",
    "/branches/{id}/artifacts",
    "/branches/{id}/issues",
    "/branches/{id}/slack/reply",
    "/branches/{id}/status",
    "/channels",
    "/channels/{id}",
    "/channels/{id}/bindings",
    "/diagnostics",
    "/env",
    "/issues",
    "/issues/{id}",
    "/logs",
    "/mcps",
    "/profiles",
    "/profiles/{name}/clone",
    "/profiles/{name}/effective",
    "/repos",
    "/repos/env",
    "/reviews/{id}/comments",
    "/reviews/{id}/retry-delivery",
    "/reviews/{id}/submit",
    "/runs",
    "/runs/{id}",
    "/scratch/limits",
    "/self",
    "/session-launches/resolve",
    "/session-layout",
    "/session-layout/groups",
    "/session-layout/moves",
    "/session-layout/reorder",
    "/session-layout/restores",
    "/session-layout/spaces",
    "/sessions/search",
    "/sessions/summary",
    "/sessions/{id}/adopt",
    "/sessions/{id}/archive",
    "/sessions/{id}/artifacts",
    "/sessions/{id}/changes",
    "/sessions/{id}/chat",
    "/sessions/{id}/conversation",
    "/sessions/{id}/files",
    "/sessions/{id}/github/access",
    "/sessions/{id}/handoff",
    "/sessions/{id}/history",
    "/sessions/{id}/history/search",
    "/sessions/{id}/ide-info",
    "/sessions/{id}/interrupt",
    "/sessions/{id}/log",
    "/sessions/{id}/mode",
    "/sessions/{id}/preview",
    "/sessions/{id}/raw",
    "/sessions/{id}/recover",
    "/sessions/{id}/send",
    "/sessions/{id}/shells",
    "/sessions/{id}/summary",
    "/sessions/{id}/tags",
    "/sessions/{id}/url",
    "/settings",
    "/shell/restart",
    "/slack/status",
    "/status",
    "/tasks",
    "/watches",
    "/watches/{id}/run",
    "/watches/{id}/runs",
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

    let superseded: BTreeSet<&str> = SUPERSEDED_ROUTES.iter().copied().collect();
    let mounted = mounted_routes();

    let unaccounted: Vec<String> = mounted
        .iter()
        .filter(|route| !transport.contains(route.as_str()))
        .filter(|route| !operations.contains(*route))
        .filter(|route| !superseded.contains(route.as_str()))
        .cloned()
        .collect();

    assert!(
        unaccounted.is_empty(),
        "{} hand-mounted routes are neither an operation, a declared transport, nor a\n\
         known superseded route. Each is either a new parallel surface (register it)\n\
         or a legacy route that belongs in SUPERSEDED_ROUTES with the rest:\n{}",
        unaccounted.len(),
        unaccounted
            .iter()
            .map(|route| format!("  {route}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // The ratchet: a superseded route that is gone must leave this list, so the
    // count is always the real remaining debt rather than a historical high-water
    // mark.
    let retired: Vec<&str> = SUPERSEDED_ROUTES
        .iter()
        .copied()
        .filter(|route| !mounted.contains(*route))
        .collect();
    assert!(
        retired.is_empty(),
        "these routes are no longer mounted — delete them from SUPERSEDED_ROUTES: {retired:?}"
    );
}

/// A hand-mounted route may not duplicate an operation's own route.
///
/// The ledger above treats "this route equals an operation's route" as
/// *accounted for*, which is exactly backwards when the route is still mounted
/// by hand: the operation dispatcher already serves that path, so the second
/// mount is either dead code shadowed by the first, or — when the methods match
/// — an axum panic at startup. `POST /deployment/reconcile` was the latter, and
/// it took down every integration test with a message about none of this.
///
/// The `io = Session` operations are the deliberate exception: they are mounted
/// by hand *instead of* by the dispatcher, because their response must carry a
/// Set-Cookie.
#[test]
fn no_hand_mounted_route_duplicates_an_operation() {
    let hand_mounted_on_purpose: BTreeSet<String> = weaver_api::operations::operations()
        .filter(|operation| !operation.io.is_json())
        .map(|operation| {
            operation
                .path()
                .strip_prefix("/api")
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    // Superseded GET routes the browser still calls. Each has an operation that
    // serves the same data at `POST /api/<id>`, and each disappears when its
    // frontend call site moves. They are pinned by name rather than tolerated by
    // rule, so a *new* duplicate still fails this test.
    let pending_frontend_migration: BTreeSet<&str> = [
        "/repos/branches",
        "/repos/recent",
        "/repos/revisions/validate",
        "/watches/programs",
    ]
    .into_iter()
    .collect();

    let operations = operation_routes();
    let duplicates: Vec<String> = mounted_routes()
        .into_iter()
        .filter(|route| operations.contains(route))
        .filter(|route| !hand_mounted_on_purpose.contains(route))
        .filter(|route| !pending_frontend_migration.contains(route.as_str()))
        .collect();

    assert!(
        duplicates.is_empty(),
        "{} routes are mounted by hand AND derived from an operation. Delete the \n\
         hand-written route; the operation already serves that path:\n{}",
        duplicates.len(),
        duplicates
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

/// How every CLI-projecting operation is actually reached.
///
/// Two paths exist on purpose: a hand-written command formats output worth
/// formatting, and the generic dispatcher makes a newly declared operation
/// reachable with no second edit. What must not happen is an operation that
/// advertises `loom foo bar` and is reachable by neither — which is exactly what
/// the previous surface shipped, three times over.
///
/// This prints the split rather than asserting a ratio; the number that must be
/// zero is the unreachable one.
#[test]
fn every_cli_operation_is_reachable() {
    let bound: std::collections::BTreeSet<&str> = loom::cli::bindings()
        .iter()
        .map(|binding| binding.operation.id)
        .collect();

    let unreachable: Vec<&str> = weaver_api::operations::operations()
        .filter(|operation| operation.cli.is_some())
        .map(|operation| operation.id)
        .filter(|id| !bound.contains(id))
        .collect();

    assert!(
        unreachable.is_empty(),
        "operations advertise a CLI invocation that nothing serves: {unreachable:?}"
    );
}
