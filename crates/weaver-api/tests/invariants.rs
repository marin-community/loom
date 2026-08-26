//! The invariants that keep the registry honest.
//!
//! Each test here corresponds to a way the previous registry drifted from the
//! surface it claimed to describe. They are cheap, they run in CI, and they are
//! the reason this design cannot be half-finished the way the last one was.

use std::collections::{BTreeMap, BTreeSet};

use weaver_api::operations::{self, Operands, ViewFlags};

/// Structural validity: unique ids, unique projections, grants present, and no
/// MCP tool on anything an agent may not call.
#[test]
fn registry_validates() {
    operations::validate_operation_registry().expect("registry must be structurally valid");
}

/// Routes are derived from identity, so every operation must resolve from its own route.
#[test]
fn routes_round_trip() {
    for operation in operations::operations() {
        let resolved = operations::operation_for_request(operation.method(), &operation.path())
            .unwrap_or_else(|| {
                panic!(
                    "{} does not resolve from {}",
                    operation.id,
                    operation.path()
                )
            });
        assert_eq!(resolved.id, operation.id);
    }
}

/// Every operation's id maps to exactly one route and vice versa.
#[test]
fn routes_are_unique() {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for operation in operations::operations() {
        let route = format!("{} {}", operation.method(), operation.path());
        if let Some(previous) = seen.insert(route.clone(), operation.id) {
            panic!("{route} is claimed by both {previous} and {}", operation.id);
        }
    }
}

/// Context fields are dispatcher-supplied and must never appear in the schema a caller reads.
#[test]
fn context_fields_are_never_caller_supplied() {
    for operation in operations::operations() {
        let schema = (operation.schema)();
        let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
            continue;
        };
        for field in operation.context {
            assert!(
                !properties.contains_key(field.name),
                "{} exposes context field `{}` to callers",
                operation.id,
                field.name
            );
        }
    }
}

/// Every declared grant follows the one capability vocabulary. The design this
/// replaces carried two (`mcp/*@v1` and `loom/*@v1`) bridged by a hand-written
/// match statement.
#[test]
fn grants_use_one_vocabulary() {
    for operation in operations::operations() {
        for grant in operation.grants {
            assert!(
                grant.starts_with("loom/") && grant.contains('@'),
                "{} declares grant `{grant}`, which is not loom/<bundle>/<verb>@vN",
                operation.id
            );
        }
    }
}

/// Every operation's schema is a well-formed object schema. A derive that
/// silently produced nothing would otherwise look like "takes no arguments".
#[test]
fn every_schema_is_an_object() {
    for operation in operations::operations() {
        let schema = (operation.schema)();
        assert_eq!(
            schema.get("type").and_then(|t| t.as_str()),
            Some("object"),
            "{} has a non-object input schema: {schema}",
            operation.id
        );
    }
}

/// A CLI projection must name at least one segment and must not collide.
#[test]
fn cli_projections_are_well_formed() {
    let mut seen: BTreeMap<Vec<&str>, &str> = BTreeMap::new();
    for operation in operations::operations() {
        let Some(cli) = operation.cli else { continue };
        assert!(
            !cli.path.is_empty(),
            "{} declares an empty CLI path",
            operation.id
        );
        let path = cli.path.to_vec();
        if let Some(previous) = seen.insert(path.clone(), operation.id) {
            panic!(
                "CLI path {path:?} is claimed by both {previous} and {}",
                operation.id
            );
        }
    }
}

/// The reference vertical, checked end to end: one declaration really does
/// produce the REST route, the MCP schema, and the clap surface.
#[test]
fn one_declaration_produces_every_projection() {
    use operations::issues;

    let spec = issues::list::SPEC;
    assert_eq!(spec.path(), "/api/issues/list");
    assert_eq!(spec.cli.unwrap().invocation(), "loom issues list");
    assert_eq!(spec.cli.unwrap().aliases, &["ls"]);

    let schema = (spec.schema)();
    assert!(schema["properties"].get("all").is_some());
    assert!(schema["properties"].get("repo_root").is_none());

    let command = <issues::list::View as ViewFlags>::augment(
        <issues::list::Input as Operands>::augment(clap::Command::new("list")),
    );
    let matches = command
        .try_get_matches_from(["list", "--all", "--mine"])
        .expect("the advertised flags must parse");
    assert!(
        <issues::list::Input as Operands>::from_matches(&matches)
            .unwrap()
            .all
    );
    assert!(
        <issues::list::View as ViewFlags>::from_matches(&matches)
            .unwrap()
            .mine
    );
}

/// The convention itself, checked: a bundle's operations live in the one file
/// named after it, and no extra file under `operations/` goes unregistered.
///
/// Every `issues.*` operation lives in `operations/issues.rs`. This is what
/// keeps the tree navigable — you read a bundle name in a log line and you
/// know the file — without a bundle's operations scattering across files or a
/// stray file drifting out of the registry unnoticed.
#[test]
fn every_bundle_has_exactly_one_file_and_nothing_else_does() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/operations");
    // A bundle's public name may use hyphens (`session-layout`); its module,
    // like every Rust identifier, cannot, so the file is the underscored form.
    let module_names: BTreeSet<String> = operations::operation_bundles()
        .map(|bundle| bundle.name.replace('-', "_"))
        .collect();

    let missing: Vec<_> = module_names
        .iter()
        .filter(|name| !root.join(format!("{name}.rs")).is_file())
        .collect();
    assert!(
        missing.is_empty(),
        "bundles with no matching operations/<name>.rs file: {missing:#?}"
    );

    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(&root).expect("operations tree is readable") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        // `mod.rs` wires up the crate; `registry.rs` is the vocabulary module.
        if stem == "mod" || stem == "registry" || module_names.contains(&stem) {
            continue;
        }
        orphans.push(stem);
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "operations/ files not named by any registered bundle: {orphans:#?}"
    );
}

/// The unauthenticated surface, pinned by explicit declaration.
///
/// Operations reachable without a credential are the highest-consequence thing
/// this registry can declare. Adding one requires a deliberate edit to this test.
#[test]
fn anonymous_operations_are_pinned() {
    let anonymous: BTreeSet<&str> = operations::operations()
        .filter(|operation| operation.actor == weaver_api::operations::ActorPolicy::Anonymous)
        .map(|operation| operation.id)
        .collect();
    // All three are what a browser needs *before* it has a credential: discover
    // the sign-in methods (`auth.me`), then use one of them.
    let expected: BTreeSet<&str> = ["auth.login", "auth.federate", "auth.me"]
        .into_iter()
        .collect();
    assert_eq!(
        anonymous, expected,
        "the set of operations reachable WITHOUT ANY CREDENTIAL changed. \
         If that is intended, update this test deliberately."
    );
}

/// Nothing anonymous may be destructive, and nothing anonymous may carry a grant.
///
/// A grant is a property of a credential; an operation that needs no credential
/// cannot meaningfully require one, and a declaration that says otherwise is
/// confused about which check protects it.
#[test]
fn anonymous_operations_are_narrow() {
    for operation in operations::operations() {
        if operation.actor != weaver_api::operations::ActorPolicy::Anonymous {
            continue;
        }
        assert!(
            operation.grants.is_empty(),
            "{} is anonymous but declares grants {:?}",
            operation.id,
            operation.grants
        );
    }
}

/// The operations no human may call, pinned.
///
/// `SessionOnly` exists for operations that return credential material, where
/// letting an operator stand in for a session means one user obtaining
/// another's token. Like the anonymous set, it is small enough to write down,
/// and writing it down is what makes widening it a visible decision.
#[test]
fn session_only_operations_are_pinned() {
    let restricted: BTreeSet<&str> = operations::operations()
        .filter(|operation| operation.actor == weaver_api::operations::ActorPolicy::SessionOnly)
        .map(|operation| operation.id)
        .collect();
    let expected: BTreeSet<&str> = ["permissions.github.token"].into_iter().collect();
    assert_eq!(
        restricted, expected,
        "the set of operations NO HUMAN may call changed. \
         Widening it hands session credential material to operators."
    );
}

/// Operations *not* served off the generic JSON dispatcher are pinned.
///
/// Each of these needs a hand-written handler, so each is a place the
/// declaration and the implementation can drift. That is a cost worth counting,
/// not a category worth hiding: `io` names *why* the handler is custom, and the
/// reasons below are the whole list.
///
/// * `session` — the response must carry a `Set-Cookie`, which the dispatcher
///   cannot emit. Served beside the auth routes.
/// * `stream` — the response is an SSE body.
/// * `duplex` — the response is a websocket upgrade.
/// * `upload` — the *request* body is the payload's raw bytes.
/// * `download` — the *response* body is raw bytes with a guessed content type,
///   because the caller is a browser fetching a URL rather than posting JSON.
///
/// Every one of these is a registered operation with a derived route,
/// a declared actor, and authorization. Operands arrive in the query string
/// for operations without a JSON body, and `loom::web::encodings` mounts all
/// of these off the declarations in this registry.
#[test]
fn transport_specific_operations_are_pinned() {
    let special: BTreeMap<&str, &str> = operations::operations()
        .filter(|operation| !operation.io.is_json())
        .map(|operation| (operation.id, operation.io.as_str()))
        .collect();
    let expected: BTreeMap<&str, &str> = [
        ("auth.federate", "session"),
        ("auth.login", "session"),
        ("auth.logout", "session"),
        ("events.stream", "stream"),
        ("logs.stream", "stream"),
        ("session_layout.events", "stream"),
        ("artifacts.raw", "download"),
        ("sessions.chat.stream", "stream"),
        ("sessions.events.stream", "stream"),
        ("sessions.raw", "download"),
        ("sessions.scratch.write", "upload"),
        ("sessions.shells.terminal", "duplex"),
        ("sessions.terminal", "duplex"),
        ("shell.terminal", "duplex"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        special, expected,
        "the set of operations NOT served by the generic dispatcher changed. \
         Every addition is an operation that has to be kept in sync by hand."
    );
}

/// An operation with no JSON request body takes its operands from the query
/// string, which axum deserializes before any dispatcher default-filling could
/// run. So every one of its operands must be optional on the wire — otherwise
/// An operation describes what it returns, not only what it takes.
///
/// A caller told exactly what to send and nothing about what comes back has to
/// write the response type out by hand — which is why `frontend/src/types.ts`
/// holds 179 interfaces mirroring these DTOs. The bytes an `io = Stream`,
/// `Duplex`, or `Download` operation emits are not JSON and have no schema;
/// everything else must have one.
#[test]
fn operations_describe_their_response() {
    for operation in operations::operations() {
        if matches!(operation.io.as_str(), "stream" | "duplex" | "download") {
            continue;
        }
        let schema = (operation.output_schema)();
        assert_ne!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("null"),
            "{} returns `()`; a caller has nothing to decode",
            operation.id,
        );
    }
}

/// the declared route 400s on a caller that named nothing, including exactly the
/// request a session credential makes when it means "my own session".
///
/// `io = Session` is exempt: it is a POST with a JSON body, and its response is
/// special only in carrying a `Set-Cookie`.
///
/// Checked here as a property of the *declaration* (the server-side counterpart,
/// which runs the real `Query` extractor, lives in `loom::web::encodings`).
#[test]
fn streaming_operations_declare_no_required_operand() {
    for operation in operations::operations() {
        if !matches!(
            operation.io.as_str(),
            "stream" | "duplex" | "upload" | "download"
        ) {
            continue;
        }
        let schema = (operation.schema)();
        let required = schema
            .get("required")
            .and_then(|value| value.as_array())
            .map(|values| values.len())
            .unwrap_or(0);
        assert_eq!(
            required,
            0,
            "{} is io={} and declares required operands {:?}; a query string \
             cannot be relied on to carry them",
            operation.id,
            operation.io.as_str(),
            schema.get("required")
        );
    }
}
