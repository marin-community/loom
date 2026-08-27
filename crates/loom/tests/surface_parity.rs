//! The transport ledger.
//!
//! [`TRANSPORT_ROUTES`] lists every hand-mounted route as a decision, not a
//! gap: eighteen paths the operation DSL cannot express, each with the reason
//! written next to it. A route in `web/mod.rs` with no operation behind it —
//! no schema, no CLI, no MCP projection, no declared actor policy — is a
//! second API surface, and this file has no bucket for "temporarily allowed."
//!
//! A new `.route(` in `web/mod.rs` that is not on this list fails
//! [`no_route_is_unaccounted_for`]. The way to add an endpoint is to declare
//! an operation; the way to add a transport is to defend it here in writing.

use std::collections::BTreeSet;

/// Routes that are deliberately NOT operations, with the reason.
///
/// Adding a line here is a decision that the operation DSL cannot express this
/// endpoint. It is not a place to park work.
const TRANSPORT_ROUTES: &[(&str, &str)] = &[
    // Streams and websockets are not here: they are registered operations with
    // `io = Stream` / `io = Duplex`, mounted by `web::encodings` at the paths
    // their ids derive — see `hand_mounted_on_purpose` below. An operand takes
    // its value from the query string regardless of encoding, so a GET stream
    // needs no special-cased path.
    //
    // Reverse proxy to the embedded editor. This one really is not an operation:
    // it forwards an arbitrary sub-path to code-server and streams whatever
    // comes back.
    ("/sessions/{id}/ide", "proxy: code-server"),
    ("/sessions/{id}/ide/", "proxy: code-server"),
    ("/sessions/{id}/ide/{*rest}", "proxy: code-server"),
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
    // surface; they are mounted by hand because their response must carry a
    // Set-Cookie, which the generic dispatcher cannot emit. They are listed here
    // *and* excluded from the duplicate check below, because their real path and
    // their derived path are the same string.
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

/// Every path `web/mod.rs` mounts by hand.
///
/// Scans the whole source rather than line by line, because rustfmt wraps a
/// long `.route(` onto its own line. A line-oriented scan missed a third of the
/// surface this way, including two websockets and an unregistered DELETE.
fn mounted_routes() -> BTreeSet<String> {
    let source = include_str!("../src/web/mod.rs");
    let mut routes = BTreeSet::new();
    for (index, _) in source.match_indices(".route(") {
        let rest = &source[index + ".route(".len()..];
        let Some(open) = rest.find('"') else { continue };
        // Only a literal that *is* the first argument counts; anything else
        // before the quote means this is not a `.route("path", …)` call.
        if !rest[..open].chars().all(char::is_whitespace) {
            continue;
        }
        if let Some((path, _)) = rest[open + 1..].split_once('"') {
            routes.insert(path.to_string());
        }
    }
    routes
}

/// The scan above is load-bearing, so it is checked against a known shape.
///
/// A line-oriented version of this scan once saw 105 of 160 routes, because
/// rustfmt wraps a long `.route(` onto its own line — a third of the surface was
/// invisible to the test whose whole job was seeing it. The wrapped IDE-proxy
/// route is the fixture for that: if the scan regresses to reading lines, this
/// fails before `no_route_is_unaccounted_for` starts passing for the wrong
/// reason.
#[test]
fn the_route_scan_sees_wrapped_route_calls() {
    let routes = mounted_routes();
    assert!(
        routes.contains("/sessions/{id}/ide/{*rest}"),
        "the route scan missed a `.route(` whose path literal is on the next line"
    );
    assert_eq!(
        routes.len(),
        TRANSPORT_ROUTES.len(),
        "the hand-mounted surface and the declared transport list have different \
         sizes, so one of them moved without the other: {routes:?}"
    );
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

/// Every hand-mounted route is a declared transport.
///
/// This is the invariant the three-bucket ledger was built to retire itself
/// into. There is no "superseded" bucket to park a route in any more: a route in
/// `web/mod.rs` that is not in [`TRANSPORT_ROUTES`] is a second API surface, and
/// the failure message says what to do about it.
///
/// [`every_declared_transport_is_mounted`] is the other direction.
#[test]
fn no_route_is_unaccounted_for() {
    let transport: BTreeSet<&str> = TRANSPORT_ROUTES.iter().map(|(path, _)| *path).collect();
    let mounted = mounted_routes();

    let unaccounted: Vec<&String> = mounted
        .iter()
        .filter(|route| !transport.contains(route.as_str()))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "{} hand-mounted routes are not declared transports. Declare an operation \
         instead — the registry derives its route, its schema, its CLI command \
         and its authority from one place — or, if this really cannot be an \
         operation, add it to TRANSPORT_ROUTES with the reason:\n{}",
        unaccounted.len(),
        unaccounted
            .iter()
            .map(|route| format!("  {route}"))
            .collect::<Vec<_>>()
            .join("\n")
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

    let operations = operation_routes();
    let duplicates: Vec<String> = mounted_routes()
        .into_iter()
        .filter(|route| operations.contains(route))
        .filter(|route| !hand_mounted_on_purpose.contains(route))
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
/// advertises `loom foo bar` and is reachable by neither.
///
/// The number that must be zero is the unreachable count; this prints the full
/// split for visibility, not just that count.
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

/// Every literal path in `grant_allows` is a route the server still mounts.
///
/// `grant_allows` decides seven raw paths — the ones that are not operations —
/// and this check does not care how short that list is: a path literal whose
/// route no longer exists is a standing **pre-authorization** for whatever gets
/// mounted at that path next, readable by every session credential in the
/// fleet before anyone decides it should be. Three entries had gone stale this
/// way: `/settings` and `/profiles` after their routes became operations, and
/// `/repos/issues`, unmounted since #309.
///
/// Scanned rather than enumerated: a hand-kept copy of this list is itself the
/// kind of thing that goes stale.
#[test]
fn no_grant_allows_path_is_unmounted() {
    check_path_literals_are_mounted(
        "let discovery = *method",
        "/// The session id an IDE-proxy path addresses",
        "reachable by a bare credential",
    );
}

/// Every `"/…"` literal between two markers in `web/auth.rs` names a mounted
/// route. Scanned rather than enumerated, for the same reason the route ledger
/// is: a hand-kept copy of the list is the thing that goes stale.
fn check_path_literals_are_mounted(from: &str, to: &str, role: &str) {
    let source = include_str!("../src/web/auth.rs");
    let start = source
        .find(from)
        .unwrap_or_else(|| panic!("web/auth.rs no longer contains `{from}`"));
    let end = source[start..]
        .find(to)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("no `{to}` after `{from}` in web/auth.rs"));
    let arm = &source[start..end];

    // Every string literal in the arm that looks like a whole path. Structural
    // checks (`segments.first() == Some(&"channels")`) name a single segment
    // without a leading slash, so they are excluded by that test rather than by
    // a list of exceptions.
    let mut claimed: BTreeSet<&str> = BTreeSet::new();
    for (index, _) in arm.match_indices('"') {
        let rest = &arm[index + 1..];
        let Some((literal, _)) = rest.split_once('"') else {
            continue;
        };
        if literal.starts_with('/') && !literal.is_empty() {
            claimed.insert(literal);
        }
    }
    assert!(
        claimed.len() >= 3,
        "the scan between `{from}` and `{to}` found only {claimed:?} — \
         it stopped matching the source"
    );

    let mounted = mounted_routes();
    // A prefix rule (`path.starts_with("/operations/")`) authorizes a family
    // whose members are mounted with a path parameter, so accept either an exact
    // route or one that extends the claim.
    let unmounted: Vec<&str> = claimed
        .iter()
        .copied()
        .filter(|claim| {
            !mounted.contains(*claim) && !mounted.iter().any(|route| route.starts_with(*claim))
        })
        .collect();
    assert!(
        unmounted.is_empty(),
        "these paths are {role} in `web/auth.rs` but no longer mounted: \
         {unmounted:?}. Delete the entry — the operation that replaced the route \
         carries the policy through `actor` instead."
    );
}
