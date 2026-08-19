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
    "/diagnostics",
    "/issues",
    "/issues/{id}",
    "/logs",
    "/repos",
    "/repos/env",
    "/reviews/{id}",
    "/reviews/{id}/comments",
    "/reviews/{id}/comments/{comment_id}",
    "/reviews/{id}/comments/{comment_id}/resolve",
    "/reviews/{id}/retarget-current",
    "/reviews/{id}/retry-delivery",
    "/reviews/{id}/submit",
    "/runs",
    "/runs/{id}",
    "/scratch/limits",
    "/self",
    "/session-launches/resolve",
    "/session-layout",
    "/session-layout/defaults",
    "/session-layout/defaults/{kind}/{value}",
    "/session-layout/groups",
    "/session-layout/groups/{id}",
    "/session-layout/groups/{id}/preference",
    "/session-layout/moves",
    "/session-layout/reorder",
    "/session-layout/restores",
    "/session-layout/spaces",
    "/session-layout/spaces/{id}",
    "/sessions/search",
    "/sessions/summary",
    "/sessions/{id}",
    "/sessions/{id}/adopt",
    "/sessions/{id}/archive",
    "/sessions/{id}/artifacts",
    "/sessions/{id}/changes",
    "/sessions/{id}/chat",
    "/sessions/{id}/config/{config_id}",
    "/sessions/{id}/conversation",
    "/sessions/{id}/conversation/blocks/{message}/{block}",
    "/sessions/{id}/files",
    "/sessions/{id}/github",
    "/sessions/{id}/github/access",
    "/sessions/{id}/github/labels",
    "/sessions/{id}/handoff",
    "/sessions/{id}/handoff/resolve",
    "/sessions/{id}/history",
    "/sessions/{id}/history/search",
    "/sessions/{id}/ide-info",
    "/sessions/{id}/interrupt",
    "/sessions/{id}/log",
    "/sessions/{id}/mode",
    "/sessions/{id}/permissions/{request_id}",
    "/sessions/{id}/preview",
    "/sessions/{id}/prompt",
    "/sessions/{id}/raw",
    "/sessions/{id}/recover",
    "/sessions/{id}/resumption-cue",
    "/sessions/{id}/reviews",
    "/sessions/{id}/send",
    "/sessions/{id}/shells",
    "/sessions/{id}/summary",
    "/sessions/{id}/tags",
    "/sessions/{id}/title-generation",
    "/sessions/{id}/title/regenerate",
    "/sessions/{id}/url",
    "/slack/status",
    "/status",
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
    "/branches/{id}/artifacts/{name}/url",
    "/branches/{id}/events",
    "/branches/{id}/tags/{key}",
    "/channels/{id}/bindings",
    "/channels/{id}/messages",
    "/channels/{id}/read-marker",
    "/channels/{id}/subscription",
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
/// that it read as zero, which is the most expensive kind of wrong number. It
/// was 26 lines then and is two now; both survivors are about a wire encoding
/// the `Io` enum does not yet serve, not about the registry lacking the shape.
///
/// A caveat this list cannot express: it is keyed by path, while an operation is
/// keyed by method *and* path. Several routes are therefore in
/// [`SUPERSEDED_ROUTES`] with one method that no operation can actually serve
/// yet. Named here so the gap is written down somewhere other than a frontend
/// comment:
///
/// * `PATCH /issues/{id}` — there is no `issues.update`; `issues.close`/
///   `.reopen` take an id array and answer a bulk `IssueActionsResult`.
/// * `GET /sessions/{id}/github/access` — grant and revoke are operations, the
///   read is not.
/// * `GET /issues` — the cross-repo, automation-aware board. `issues.list` is
///   `scope = Repository` and has no `automation` filter: a different read.
/// * `GET /sessions/summary` — `sessions.list` answers `SessionView[]`, not the
///   reduced `SessionSummary[]` projection, and this caller needs an
///   `AbortSignal` the operation client does not thread.
/// * `POST /sessions` — `sessions.launch` declares `title` as a required
///   `String`, but the route it replaces takes `Option<String>` and *derives*
///   the title from the claimed issue (`title_provenance = "derived"`, asserted
///   in `e2e/tests/create.spec.ts`). The declaration is stricter than the route,
///   which is the same defect `settings.patch` had; making the operand optional
///   is a CLI contract change, so it is written down rather than guessed at.
/// * `POST /channels` — `channels.create` is `scope = Branch` with a `branch`
///   context field, and the dashboard's picker holds only a repo root.
/// * `POST /issues` — `issues.backlog.create`'s `Input` has no `tags` field, so
///   it cannot express the initial tag set the route accepts.
const UNREGISTERED_ROUTES: &[(&str, &str)] = &[
    (
        "/sessions/{id}/artifacts/{name}/raw",
        "artifacts: the response is image bytes, and `Io` has no variant for a \
         raw binary body — adding one is a design decision, not a port",
    ),
    (
        "/sessions/{id}/scratch",
        "sessions: three methods on one path. GET (list) and DELETE are ordinary \
         JSON and could be registered today; POST takes a raw octet-stream body, \
         which is what `Io::Upload` is for — it is declared and so far unused",
    ),
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

/// Every literal path in the session-credential allowlist is a route the server
/// still mounts.
///
/// `grant_allows` in `src/web/auth.rs` decides a *raw path* — it is the legacy
/// authority model, and it now applies only to routes that are not operations.
/// So when a route becomes an operation, its entry there has to go, and two did
/// not: `/settings` and `/profiles` sat in the bare-GET list for a while after
/// their routes were deleted.
///
/// A stale entry is worse than dead code. It is a *pre-authorization* for a path
/// nothing serves — so the day someone mounts something else at `/settings`,
/// every session credential in the fleet can already read it, and nothing in
/// this repository would have said so.
///
/// Scanned rather than enumerated, for the same reason the route ledger is: a
/// hand-kept copy of this list is the thing that goes stale.
#[test]
fn no_session_allowlist_path_is_unmounted() {
    check_path_literals_are_mounted(
        "Grant::Session {",
        "pub(super) fn operation_grant_allows",
        "pre-authorized for session credentials",
    );
}

/// The same check for the human-operator deny-list.
///
/// A stale entry here is the milder direction — a dead *denial* rather than a
/// dead permission — but it still reads as policy that is being enforced when it
/// is not, and it is how you end up believing `/settings` is admin-gated by this
/// function when the real gate is `settings.patch`'s `actor`.
#[test]
fn no_user_denylist_path_is_unmounted() {
    check_path_literals_are_mounted(
        "fn user_grant_allows(",
        "\n/// ",
        "denied to non-admin humans",
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
        claimed.len() > 3,
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
