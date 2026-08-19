//! The invariants that keep the registry honest.
//!
//! Each test here corresponds to a way the previous registry drifted from the
//! surface it claimed to describe. They are cheap, they run in CI, and they are
//! the reason this design cannot be half-finished the way the last one was.

use std::collections::{BTreeMap, BTreeSet};

use weaver_api::operations::{self, Operands, Operation, ViewFlags};

/// Structural validity: unique ids, unique projections, grants present, and no
/// MCP tool on anything an agent may not call.
#[test]
fn registry_validates() {
    operations::validate_operation_registry().expect("registry must be structurally valid");
}

/// Invariant 4 (the half of it that is a security property).
///
/// "An agent cannot approve its own permission request" used to be an *absence*
/// — you verified it by failing to find a tool. Now it is a checked property.
#[test]
fn only_agent_reachable_operations_expose_mcp_tools() {
    let leaked: Vec<_> = operations::operations()
        .filter(|operation| operation.mcp.is_some() && !operation.actor.agent_reachable())
        .map(|operation| (operation.id, operation.actor.as_str()))
        .collect();
    assert!(
        leaked.is_empty(),
        "human-only operations must not be agent-reachable: {leaked:?}"
    );
}

/// A non-JSON operation cannot be served by the schema-driven MCP dispatcher, so
/// it must not advertise a tool. This is what stops `io` from being a quiet way
/// to smuggle an operation out of the generic path.
#[test]
fn only_json_operations_expose_mcp_tools() {
    let leaked: Vec<_> = operations::operations()
        .filter(|operation| operation.mcp.is_some() && !operation.io.is_json())
        .map(|operation| (operation.id, operation.io.as_str()))
        .collect();
    assert!(leaked.is_empty(), "non-JSON operations with MCP tools: {leaked:?}");
}

/// Invariant 3: the MCP catalogue and the registry are the same set.
#[test]
fn mcp_catalogue_is_a_bijection_with_the_registry() {
    let mut declared: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for operation in operations::operations() {
        if let Some(mcp) = operation.mcp {
            declared.entry(mcp.server).or_default().insert(mcp.tool);
        }
    }
    for (server, tools) in &declared {
        let generated = operations::mcp_tools(server);
        let generated: BTreeSet<&str> = generated
            .as_array()
            .expect("catalogue is an array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool has a name"))
            .collect();
        let expected: BTreeSet<&str> = tools.iter().copied().collect();
        assert_eq!(
            generated, expected,
            "MCP server {server} catalogue disagrees with the registry"
        );
    }
}

/// Routes are derived from identity, so every operation resolves from its own
/// route. The old registry declared a route *and* computed one, and they
/// disagreed for every generated operation.
#[test]
fn routes_round_trip() {
    for operation in operations::operations() {
        let resolved = operations::operation_for_request(operation.method(), &operation.path())
            .unwrap_or_else(|| panic!("{} does not resolve from {}", operation.id, operation.path()));
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

/// Context fields are dispatcher-supplied and must never appear in the schema a
/// caller reads. This is the fix for the old `args` / `Input` split, where the
/// MCP schema and the REST body described different shapes.
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
            panic!("CLI path {path:?} is claimed by both {previous} and {}", operation.id);
        }
    }
}

/// The reference vertical, checked end to end: one declaration really does
/// produce the REST route, the MCP schema, and the clap surface.
#[test]
fn one_declaration_produces_every_projection() {
    use operations::issues;

    let spec = <issues::list::List as Operation>::SPEC;
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
    assert!(<issues::list::Input as Operands>::from_matches(&matches)
        .unwrap()
        .all);
    assert!(<issues::list::View as ViewFlags>::from_matches(&matches)
        .unwrap()
        .mine);
}

/// The convention itself, checked: an operation's id is its path on disk.
///
/// `issues.tags.set` lives in `operations/issues/tags/set.rs` and nowhere else.
/// This is what makes the tree navigable without a map — you read an id in a log
/// line and you know the file. A registry that allows `issues.rs` to quietly
/// accumulate twelve operations drifts back into the 1096-line module this
/// replaced.
#[test]
fn every_operation_lives_in_the_file_its_id_names() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/operations");
    let mut missing = Vec::new();
    for operation in operations::operations() {
        let relative = format!("{}.rs", operation.id.replace('.', "/"));
        if !root.join(&relative).is_file() {
            missing.push((operation.id, relative));
        }
    }
    assert!(
        missing.is_empty(),
        "operations whose declaration is not in the file their id names: {missing:#?}"
    );
}

/// The other direction: no operation file is orphaned.
///
/// A file under `operations/` that declares no registered operation is either a
/// bundle `mod.rs`, or it is dead code that still compiles — which is how the
/// previous registry ended up advertising three CLI commands that did not exist.
#[test]
fn every_operation_file_declares_a_registered_operation() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/operations");
    let declared: BTreeSet<String> = operations::operations()
        .map(|operation| format!("{}.rs", operation.id.replace('.', "/")))
        .collect();

    let mut orphans = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).expect("operations tree is readable") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("path is under the operations tree")
                .to_string_lossy()
                .replace('\\', "/");
            // `mod.rs` files carry bundle wiring; `registry.rs` is the vocabulary.
            if relative.ends_with("mod.rs") || relative == "registry.rs" {
                continue;
            }
            if !declared.contains(&relative) {
                orphans.push(relative);
            }
        }
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "operation files that declare nothing the registry knows about: {orphans:#?}"
    );
}

/// The unauthenticated surface, pinned.
///
/// An operation reachable without a credential is the highest-consequence thing
/// this registry can declare, so the set is written out here rather than merely
/// validated by shape. Adding one is a deliberate edit to a test whose name says
/// what it is guarding.
#[test]
fn anonymous_operations_are_pinned() {
    let anonymous: BTreeSet<&str> = operations::operations()
        .filter(|operation| operation.actor == weaver_api::operations::ActorPolicy::Anonymous)
        .map(|operation| operation.id)
        .collect();
    let expected: BTreeSet<&str> = ["auth.login", "auth.federate"].into_iter().collect();
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
        assert!(
            operation.mcp.is_none(),
            "{} is anonymous and must not expose an MCP tool",
            operation.id
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
