//! The parallel-surface ledger.
//!
//! This file used to sort the server's hand-mounted routes into three buckets:
//! transports the operation DSL cannot express, routes an operation had already
//! superseded, and routes no operation served at all. The middle bucket was the
//! debt — 115 URLs whose data was already reachable at `POST /api/<id>`, kept
//! alive by callers that had not moved — and the third was worse, because a
//! route with no operation is a piece of API the registry does not describe:
//! no schema, no CLI, no MCP projection, no declared actor policy.
//!
//! Both are gone. Every caller moved (the browser, the CLI, the typed client,
//! the Python client, the integration and Playwright suites), the eleven
//! operations the last routes needed were declared, and the routes were deleted.
//! What remains is [`TRANSPORT_ROUTES`]: nineteen paths that are *not*
//! operations on purpose, each with the reason written next to it.
//!
//! So the ledger's job changed. It is no longer a debt counter that has to reach
//! zero — it is the assertion that the count stays zero. A new `.route(` in
//! `web/mod.rs` that is not a declared transport fails
//! [`no_route_is_unaccounted_for`], which is the whole point: the way to add an
//! endpoint is to declare an operation, and the way to add a transport is to
//! defend it here in writing.

use std::collections::BTreeSet;

/// Routes that are deliberately NOT operations, with the reason.
///
/// Adding a line here is a decision that the operation DSL cannot express this
/// endpoint. It is not a place to park work.
const TRANSPORT_ROUTES: &[(&str, &str)] = &[
    // Streams and websockets used to be listed here, on the theory that a GET
    // stream needs its arguments in the URL and so could not sit at a derived
    // path. That was wrong twice over: an operand is an operand regardless of
    // encoding, and `?session=…` is a URL. All eight are registered operations
    // now with `io = Stream` / `io = Duplex`, mounted by `web::encodings` at the
    // paths their ids derive — see `hand_mounted_on_purpose` below.
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
/// Scanned across the whole source rather than line by line: rustfmt wraps a
/// long `.route(` onto its own line, and a line-oriented scan therefore saw only
/// 105 of the 160 routes — a third of the surface was invisible to the ledger
/// below, including two websockets and an unregistered DELETE.
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

    // The other direction: a transport that stopped being mounted must leave the
    // list. A stale entry here is an endpoint the ledger claims to have thought
    // about and no longer describes anything.
    let retired: Vec<&str> = TRANSPORT_ROUTES
        .iter()
        .map(|(path, _)| *path)
        .filter(|route| !mounted.contains(*route))
        .collect();
    assert!(
        retired.is_empty(),
        "these transports are no longer mounted — delete them from the ledger: {retired:?}"
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

/// Every literal path in `grant_allows` is a route the server still mounts.
///
/// `grant_allows` decides *raw paths*, and it now decides only the seven that
/// are not operations. The check survives the collapse because the failure mode
/// it catches has nothing to do with how long the list is: a path literal there
/// whose route no longer exists is a standing **pre-authorization** for whatever
/// gets mounted at that path next, readable by every session credential in the
/// fleet before anyone decides it should be. Three entries had gone stale that
/// way — `/settings` and `/profiles` after their routes became operations, and
/// `/repos/issues`, unmounted since #309.
///
/// Its twin, `no_user_denylist_path_is_unmounted`, is gone with the list it
/// checked: every path `user_grant_allows` denied to a non-admin human is now an
/// operation carrying `actor = Admin`, which is the same rule stated once.
///
/// Scanned rather than enumerated, for the same reason the route ledger is: a
/// hand-kept copy of this list is the thing that goes stale.
#[test]
fn no_grant_allows_path_is_unmounted() {
    check_path_literals_are_mounted(
        "let discovery = *method",
        "/// The session id an IDE-proxy path addresses",
        "reachable by a bare credential",
    );
}

/// Every channel operation checks that the caller may reach the channel.
///
/// This is the one resource check `Scoped` cannot state. A channel operation is
/// addressed by channel id, but its declared `scope = Branch` names a *context*
/// operand — filled from the caller's own branch — so for a session credential
/// the scope check passes whatever channel was named. The real check is
/// `require_channel` inside `web/channels.rs`, and it was missing from
/// `channels.get` and `channels.wait`: they read the store directly, so a
/// session token could read a sibling session's channel. The legacy path
/// allowlist had been doing that check, invisibly, until the routes went away.
///
/// Counted rather than inspected per-handler: a new channel operation that takes
/// a `channel` operand and forgets the call moves one side of this equality.
#[test]
fn every_channel_operation_checks_reachability() {
    let addressed = weaver_api::operations::operations()
        .filter(|operation| operation.id.starts_with("channels."))
        .filter(|operation| {
            (operation.schema)()["properties"]
                .get("channel")
                .is_some()
        })
        .count();
    let checks = include_str!("../src/web/channels.rs")
        .matches("require_channel(&st, &principal,")
        .count();
    assert_eq!(
        checks, addressed,
        "{addressed} channel operations are addressed by channel id but \
         `require_channel` is called {checks} times. Every one of them must go \
         through it — `scope = Branch` cannot decide this, because `branch` is a \
         context operand and always names the caller's own."
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
