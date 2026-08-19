//! The parallel-surface ledger.
//!
//! Every `.route(...)` the server mounts by hand falls in exactly one of three
//! buckets, and each bucket means something different:
//!
//! * [`TRANSPORT_ROUTES`] — genuinely not an operation. A reverse proxy, an
//!   unauthenticated probe, an HMAC-authenticated webhook, an OAuth redirect.
//!   Adding a line here is a decision, not a deferral.
//! * [`SUPERSEDED_ROUTES`] — an operation already serves this data at
//!   `POST /api/<id>`; the route is still mounted only because something still
//!   calls it. This number has to reach zero.
//! * [`UNREGISTERED_ROUTES`] — no operation serves this yet. This is the
//!   registry being *incomplete*, which is a different and worse thing than
//!   being duplicated, and it is why the two are counted separately instead of
//!   both being called "legacy".
//!
//! The previous registry had no such ledger, which is how it ended up declaring
//! operations whose routes the server had stopped serving. Here the lists are
//! written down, so "we still have two surfaces" is a number that has to go to
//! zero rather than an impression.

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
    // now with `io = Stream` / `io = Duplex`, mounted by `web::streams` at the
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

/// Legacy routes an operation has superseded, still mounted for the browser.
///
/// Every one of these has a registered operation serving the same data at
/// `POST /api/<id>`; what keeps them alive is `crates/loom/frontend`, which still
/// calls them. This list is a ratchet, not a parking space: a route that is no
/// longer mounted must be deleted from it, and a hand-mounted route that is not
/// here fails [`no_route_is_unaccounted_for`]. The number that has to reach zero
/// is 77.
const SUPERSEDED_ROUTES: &[&str] = &[
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
    "/env",
    "/issues",
    "/issues/{id}",
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
    "/tasks",
    "/watches",
    "/watches/{id}/run",
    "/watches/{id}/runs",
    // Exposed by fixing the route scan: rustfmt had wrapped these onto their own
    // lines, so a third of the surface was invisible to this ledger.
    "/agents/custom/{name}",
    "/auth/federations",
    "/auth/github-token",
    "/auth/github/config",
    "/auth/users/{username}/role",
    "/branches/{id}/artifacts/{name}",
    "/branches/{id}/artifacts/{name}/threads",
    "/branches/{id}/artifacts/{name}/threads/{tid}/comments",
    "/branches/{id}/artifacts/{name}/threads/{tid}/resolve",
    "/branches/{id}/events",
    "/branches/{id}/tags/{key}",
    "/channels/{id}/messages",
    "/channels/{id}/read-marker",
    "/channels/{id}/subscription",
    "/env/{name}",
    "/mcps/custom",
    "/mcps/custom/{*identity}",
    "/profiles/{name}",
    "/profiles/{profile}/env/{name}",
    "/repos/env/{name}",
    "/sessions",
    "/sessions/{id}/artifacts/{name}",
    "/sessions/{id}/artifacts/{name}/threads",
    "/sessions/{id}/artifacts/{name}/threads/{tid}/comments",
    "/sessions/{id}/artifacts/{name}/threads/{tid}/resolve",
    "/sessions/{id}/shell/{idx}",
    "/sessions/{id}/tags/{key}",
    "/watches/{id}",
];

/// Routes with no operation behind them at all.
///
/// Distinct from [`SUPERSEDED_ROUTES`] on purpose. A superseded route is
/// *duplicated* work — an operation already serves that data and the URL is
/// waiting on its last caller. A route here is *missing* work: the registry does
/// not describe this part of the API, so it has no schema, no CLI, no MCP
/// projection, and no declared actor policy. The rule the registry exists to
/// enforce is "anything that reaches the API is registered", and every line
/// below is a place that is not yet true.
///
/// This list became visible only when the route scan above was fixed. Before
/// that it read as zero, which is the most expensive kind of wrong number.
/// A caveat this list cannot express: it is keyed by path, while an operation is
/// keyed by method *and* path. Two routes are therefore in [`SUPERSEDED_ROUTES`]
/// with one method still unregistered — `PATCH /issues/{id}` (there is no
/// `issues.update`; close/reopen return a bulk shape) and
/// `GET /sessions/{id}/github/access` (grant and revoke are operations, the read
/// is not). Both are named here so the gap is written down somewhere.
const UNREGISTERED_ROUTES: &[(&str, &str)] = &[
    ("/agent/oneshot", "agents: one-shot ACP prompt"),
    (
        "/branches/{id}/artifacts/{name}/url",
        "artifacts: share link",
    ),
    ("/channels/{id}/bindings", "channels: external bindings"),
    ("/diagnostics", "diagnostics snapshot"),
    (
        "/logs",
        "server log snapshot (logs.stream is the live half)",
    ),
    ("/preferences", "operator UI preferences"),
    ("/reviews/{id}", "reviews: read / update / discard"),
    (
        "/reviews/{id}/comments/{comment_id}",
        "reviews: edit comment",
    ),
    (
        "/reviews/{id}/comments/{comment_id}/resolve",
        "reviews: resolve comment",
    ),
    ("/reviews/{id}/retarget-current", "reviews: retarget"),
    ("/session-layout/defaults", "layout: placement default"),
    (
        "/session-layout/defaults/{kind}/{value}",
        "layout: clear placement default",
    ),
    (
        "/session-layout/groups/{id}",
        "layout: update / delete group",
    ),
    (
        "/session-layout/groups/{id}/preference",
        "layout: group preference",
    ),
    (
        "/session-layout/spaces/{id}",
        "layout: update / delete space",
    ),
    (
        "/sessions/{id}",
        "sessions: patch / delete (GET is sessions.get)",
    ),
    (
        "/sessions/{id}/artifacts/{name}/raw",
        "artifacts: image bytes",
    ),
    (
        "/sessions/{id}/config/{config_id}",
        "sessions: agent config option",
    ),
    (
        "/sessions/{id}/conversation/blocks/{message}/{block}",
        "sessions: one conversation block",
    ),
    (
        "/sessions/{id}/github",
        "sessions: PR link refresh / set / clear",
    ),
    ("/sessions/{id}/github/labels", "sessions: add PR labels"),
    (
        "/sessions/{id}/handoff/resolve",
        "sessions: preview a handoff",
    ),
    (
        "/sessions/{id}/permissions/{request_id}",
        "sessions: answer a live ACP permission prompt",
    ),
    (
        "/sessions/{id}/prompt",
        "sessions: queue / retract a prompt",
    ),
    ("/sessions/{id}/resumption-cue", "sessions: resumption cue"),
    (
        "/sessions/{id}/reviews",
        "reviews: list / create for a session",
    ),
    (
        "/sessions/{id}/scratch",
        "sessions: scratch upload (multipart)",
    ),
    (
        "/sessions/{id}/title-generation",
        "sessions: title generation toggle",
    ),
    (
        "/sessions/{id}/title/regenerate",
        "sessions: regenerate title",
    ),
    ("/slack/status", "slack: connection status"),
    ("/status", "server status"),
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
#[test]
fn the_route_scan_sees_wrapped_route_calls() {
    let routes = mounted_routes();
    assert!(
        routes.contains("/sessions/{id}/shell/{idx}"),
        "the route scan missed a `.route(` whose path literal is on the next line"
    );
    assert!(
        routes.len() > 120,
        "only {} routes found; the scan is probably broken",
        routes.len()
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

/// Every hand-mounted route is either an operation's own route or a declared
/// transport. Anything else is a surface the registry does not know about.
#[test]
fn no_route_is_unaccounted_for() {
    let transport: BTreeSet<&str> = TRANSPORT_ROUTES.iter().map(|(path, _)| *path).collect();
    let operations = operation_routes();

    let superseded: BTreeSet<&str> = SUPERSEDED_ROUTES.iter().copied().collect();
    let unregistered: BTreeSet<&str> = UNREGISTERED_ROUTES.iter().map(|(path, _)| *path).collect();
    let mounted = mounted_routes();

    let unaccounted: Vec<String> = mounted
        .iter()
        .filter(|route| !transport.contains(route.as_str()))
        .filter(|route| !operations.contains(*route))
        .filter(|route| !superseded.contains(route.as_str()))
        .filter(|route| !unregistered.contains(route.as_str()))
        .cloned()
        .collect();

    assert!(
        unaccounted.is_empty(),
        "{} hand-mounted routes are in none of the three buckets. Each is either an\n\
         operation waiting to be declared (UNREGISTERED_ROUTES), a route an\n\
         operation already serves (SUPERSEDED_ROUTES), or something the DSL cannot\n\
         express (TRANSPORT_ROUTES):\n{}",
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
        .chain(UNREGISTERED_ROUTES.iter().map(|(path, _)| *path))
        .filter(|route| !mounted.contains(*route))
        .collect();
    assert!(
        retired.is_empty(),
        "these routes are no longer mounted — delete them from the ledger: {retired:?}"
    );

    // A route cannot be both superseded and unregistered; that would mean the
    // ledger disagrees with itself about whether an operation exists.
    let both: Vec<&str> = superseded.intersection(&unregistered).copied().collect();
    assert!(both.is_empty(), "routes in two buckets at once: {both:?}");

    // And an operation must not be listed as unregistered.
    let contradicted: Vec<&str> = unregistered
        .iter()
        .copied()
        .filter(|route| operations.contains(*route))
        .collect();
    assert!(
        contradicted.is_empty(),
        "UNREGISTERED_ROUTES names routes an operation already serves — move them \
         to SUPERSEDED_ROUTES: {contradicted:?}"
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
