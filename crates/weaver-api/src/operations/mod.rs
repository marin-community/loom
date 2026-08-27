//! Loom's operation registry.
//!
//! Every operation that reaches the Loom API is declared here exactly once, and
//! REST, the CLI, MCP, and the SPA are generated from those declarations rather
//! than maintained as parallel catalogues. The rule is short enough to state in
//! one line:
//!
//! > Anything that reaches the API is registered. The only axis that varies is
//! > response encoding.
//!
//! *Who* may call an operation is [`ActorPolicy`], a field — administrative and
//! human-only actions are registered with `Admin`/`User`, not omitted. *How* it
//! answers is [`Io`], also a field — streams and uploads keep their descriptor,
//! typed input, and authorization, and differ only in encoding.
//!
//! Commands that never reach the API (`loom server run`, `setup`, shell
//! completions, the server-free half of `config`) are not operations and have no
//! entry here.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod registry;
pub use registry::*;

/// What an operation module imports. Keeping it one glob makes the per-operation
/// files short enough that the declaration is the first thing you read.
pub mod prelude {
    pub use super::registry::*;
    pub use crate::dto::*;
    pub use loom_api_macros::{operation, Operands, View};
    pub use schemars::JsonSchema;
    pub use serde::{Deserialize, Serialize};
}

pub mod agents;
pub mod artifacts;
pub mod auth;
pub mod branches;
pub mod channels;
pub mod deployment;
pub mod diagnostics;
pub mod events;
pub mod issues;
pub mod logs;
pub mod mcps;
pub mod permissions;
pub mod preferences;
pub mod profiles;
pub mod repos;
pub mod reviews;
pub mod runs;
pub mod session_layout;
pub mod sessions;
pub mod settings;
pub mod shell;
pub mod slack;
pub mod tasks;
pub mod watches;

/// One first-party resource group.
#[derive(Debug, Clone, Copy)]
pub struct OperationBundle {
    pub name: &'static str,
    pub label: &'static str,
    pub operations: &'static [&'static OperationSpec],
}

pub type OperationBundleFactory = fn() -> OperationBundle;

pub static OPERATION_BUNDLE_FACTORIES: &[OperationBundleFactory] = &[
    issues::bundle,
    artifacts::bundle,
    channels::bundle,
    sessions::bundle,
    permissions::bundle,
    watches::bundle,
    runs::bundle,
    tasks::bundle,
    settings::bundle,
    profiles::bundle,
    deployment::bundle,
    mcps::bundle,
    auth::bundle,
    agents::bundle,
    branches::bundle,
    repos::bundle,
    reviews::bundle,
    session_layout::bundle,
    events::bundle,
    logs::bundle,
    shell::bundle,
    diagnostics::bundle,
    slack::bundle,
    preferences::bundle,
];

pub fn operation_bundles() -> impl Iterator<Item = OperationBundle> {
    OPERATION_BUNDLE_FACTORIES.iter().map(|factory| factory())
}

pub fn operations() -> impl Iterator<Item = &'static OperationSpec> {
    operation_bundles().flat_map(|bundle| bundle.operations.iter().copied())
}

pub fn operations_for_bundle(bundle: &str) -> impl Iterator<Item = &'static OperationSpec> + '_ {
    operations().filter(move |operation| operation.bundle == bundle)
}

pub fn operation(id: &str) -> Option<&'static OperationSpec> {
    operations().find(|operation| operation.id == id)
}

/// Resolve a canonical operation route back to its descriptor.
///
/// Routes are derived from identity, so this is an exact inverse of
/// [`OperationSpec::path`] rather than a pattern match over a route table.
pub fn operation_for_request(method: &str, path: &str) -> Option<&'static OperationSpec> {
    let path = path.strip_prefix("/api").unwrap_or(path);
    operations().find(|operation| {
        operation.method() == method
            && operation
                .path()
                .strip_prefix("/api")
                .is_some_and(|candidate| candidate == path)
    })
}

pub fn operation_input_schema(operation: &OperationSpec) -> Value {
    (operation.schema)()
}

// -- Discovery views ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct OperationView {
    pub id: String,
    pub bundle: String,
    pub summary: String,
    pub actor: ActorPolicy,
    pub scope: OperationScope,
    pub risk: OperationRisk,
    pub io: Io,
    pub method: String,
    pub path: String,
    pub cli: Option<String>,
    pub cli_aliases: Vec<String>,
    pub grants: Vec<String>,
    pub schema: Value,
    pub output_schema: Value,
}

impl From<&OperationSpec> for OperationView {
    fn from(spec: &OperationSpec) -> Self {
        Self {
            id: spec.id.to_string(),
            bundle: spec.bundle.to_string(),
            summary: spec.summary.to_string(),
            actor: spec.actor,
            scope: spec.scope,
            risk: spec.risk,
            io: spec.io,
            method: spec.method().to_string(),
            path: spec.path(),
            cli: spec.cli.map(|cli| cli.invocation()),
            cli_aliases: spec
                .cli
                .map(|cli| {
                    cli.aliases
                        .iter()
                        .map(|alias| (*alias).to_string())
                        .collect()
                })
                .unwrap_or_default(),
            grants: spec
                .grants
                .iter()
                .map(|grant| (*grant).to_string())
                .collect(),
            schema: (spec.schema)(),
            output_schema: (spec.output_schema)(),
        }
    }
}

pub fn operation_views() -> Vec<OperationView> {
    operations().map(OperationView::from).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiMetaView {
    pub product: String,
    pub version: String,
    pub operation_registry_version: u32,
    pub operations_url: String,
    pub openapi_url: String,
}

// -- Capabilities ------------------------------------------------------------

/// Every grant named by a session-reachable operation.
pub fn all_session_capabilities() -> Vec<String> {
    let mut grants = operations()
        .filter(|operation| operation.actor.agent_reachable())
        .flat_map(|operation| operation.grants.iter().map(|grant| (*grant).to_string()))
        .collect::<Vec<_>>();
    grants.sort();
    grants.dedup();
    grants
}

// -- Validation --------------------------------------------------------------

/// Enforce the registry's structural invariants.
///
/// Runs at server startup as well as in tests.
pub fn validate_operation_registry() -> Result<(), String> {
    let mut bundle_names = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut cli_paths = std::collections::BTreeSet::new();

    for bundle in operation_bundles() {
        if !bundle_names.insert(bundle.name) {
            return Err(format!("duplicate operation bundle {}", bundle.name));
        }
        if bundle.operations.is_empty() {
            return Err(format!("operation bundle {} is empty", bundle.name));
        }
        for operation in bundle.operations {
            if operation.bundle != bundle.name {
                return Err(format!(
                    "operation {} declares bundle {} but was registered under {}",
                    operation.id, operation.bundle, bundle.name
                ));
            }
            if !ids.insert(operation.id) {
                return Err(format!("duplicate operation id {}", operation.id));
            }
            if let Some(cli) = operation.cli {
                if !cli_paths.insert(cli.path) {
                    return Err(format!("duplicate CLI projection {}", cli.invocation()));
                }
            }
            if operation.grants.is_empty() && operation.actor.agent_reachable() {
                return Err(format!(
                    "session-reachable operation {} names no grant",
                    operation.id
                ));
            }
            check_operation(operation)?;
        }
    }
    Ok(())
}

/// The properties one declaration has to satisfy on its own.
fn check_operation(operation: &OperationSpec) -> Result<(), String> {
    let fail = |why: String| Err(format!("operation {}: {why}", operation.id));

    for grant in operation.grants {
        // One capability vocabulary.
        if !grant.starts_with("loom/") || !grant.contains('@') {
            return fail(format!("grant `{grant}` is not loom/<bundle>/<verb>@vN"));
        }
    }
    // A grant is a property of a credential, so an operation that needs none
    // cannot require one — a declaration saying otherwise is confused about
    // which check protects it.
    if operation.actor == ActorPolicy::Anonymous && !operation.grants.is_empty() {
        return fail(format!(
            "is anonymous but declares grants {:?}",
            operation.grants
        ));
    }
    if operation.cli.is_some_and(|cli| cli.path.is_empty()) {
        return fail("declares an empty CLI path".to_string());
    }

    let schema = (operation.schema)();
    // A derive that silently produced nothing would otherwise read as "takes
    // no arguments".
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return fail(format!("has a non-object input schema: {schema}"));
    }
    // Context is dispatcher-supplied, so it must not appear in the schema a
    // caller reads.
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for field in operation.context {
            if properties.contains_key(field.name) {
                return fail(format!("exposes context field `{}`", field.name));
            }
        }
    }

    // No JSON request body: operands arrive in the query string, which axum
    // deserializes before any default-filling runs. A required operand there
    // 400s the caller that named nothing — including a session credential
    // meaning "my own session".
    let io = operation.io.as_str();
    if matches!(io, "stream" | "duplex" | "upload" | "download") {
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if required > 0 {
            return fail(format!(
                "is io={io} and declares required operands {:?}; a query string \
                 cannot be relied on to carry them",
                schema.get("required")
            ));
        }
    }
    // Everything whose response is JSON says what that JSON is, so no caller
    // writes the response type out by hand.
    if !matches!(operation.io.as_str(), "stream" | "duplex" | "download")
        && (operation.output_schema)()
            .get("type")
            .and_then(Value::as_str)
            == Some("null")
    {
        return fail("returns `()`; a caller has nothing to decode".to_string());
    }
    Ok(())
}

// -- OpenAPI document ---------------------------------------------------------

/// Where a hoisted schema lives in the document.
const COMPONENTS: &str = "#/components/schemas/";

/// A schemars root carries `$schema`, `$defs`, and `title`, which its `$defs`
/// twin does not. Drop them so a type reached both ways compares equal.
fn schema_body(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !matches!(key.as_str(), "$schema" | "$defs" | "title"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A schemars title that names a type the document can share.
///
/// `Input` and `Output` are the per-operation structs the macro generates, so
/// the name says nothing about the type; `Array_of_*` and `null` are structural.
fn is_shared_name(title: &str) -> bool {
    !matches!(title, "Input" | "Output" | "null" | "AnyValue")
        && !title.starts_with("Array_of_")
        && title
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_uppercase())
        && title.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `sessions.summary.list` -> `SessionsSummaryList`.
fn pascal(id: &str) -> String {
    id.split(['.', '_'])
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

fn rewrite_refs(value: &mut Value, resolve: &dyn Fn(&str) -> String) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                .map(str::to_string)
            {
                map.insert(
                    "$ref".to_string(),
                    json!(format!("{COMPONENTS}{}", resolve(&name))),
                );
            }
            for child in map.values_mut() {
                rewrite_refs(child, resolve);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_refs(item, resolve);
            }
        }
        _ => {}
    }
}

/// Every type the registry's schemas share, hoisted into one namespace.
///
/// schemars renders each schema standalone, so every operation that mentions a
/// `TagView` carries its own copy of it under `$defs`. Left alone that is 660
/// definitions of 119 distinct types in one document — and a code generator
/// walking it would emit each type as many times as it is repeated. Hoisting
/// gives every type one definition and every mention a `$ref` to it.
///
/// The name each operation's schema must use for a type whose Rust name is not
/// unique across the registry, keyed by `(operation, Rust name)`.
type SchemaNames = BTreeMap<(&'static str, String), String>;

/// Returns the `components/schemas` map plus that renaming.
fn shared_schemas() -> (serde_json::Map<String, Value>, SchemaNames) {
    // Rust name -> every (operation, body) the registry emits it with.
    let mut claims: BTreeMap<String, Vec<(&'static str, Value)>> = BTreeMap::new();
    for operation in operations() {
        for schema in [(operation.schema)(), (operation.output_schema)()] {
            if let Some(defs) = schema.get("$defs").and_then(Value::as_object) {
                for (name, body) in defs {
                    claims
                        .entry(name.clone())
                        .or_default()
                        .push((operation.id, body.clone()));
                }
            }
            // A response is one named type more often than not; hoisting the
            // root too keeps `SessionView` from appearing inline a dozen times
            // beside the copy already under `$defs`.
            if let Some(title) = schema.get("title").and_then(Value::as_str) {
                if is_shared_name(title) {
                    claims
                        .entry(title.to_string())
                        .or_default()
                        .push((operation.id, schema_body(&schema)));
                }
            }
        }
    }

    let mut names: SchemaNames = BTreeMap::new();
    let mut claimed: Vec<(String, &'static str, Value)> = Vec::new();
    for (name, sightings) in &claims {
        let distinct: BTreeSet<String> =
            sightings.iter().map(|(_, body)| body.to_string()).collect();
        for (operation, body) in sightings {
            // Every operation's input struct is named `Input`, so an operation
            // input embedded in another operation's schema claims a name that
            // is not unique. Qualifying by the operation that mentions it keeps
            // hoisting from silently merging two different types.
            let declared = if distinct.len() == 1 {
                name.clone()
            } else {
                format!("{}Nested{name}", pascal(operation))
            };
            names.insert((operation, name.clone()), declared.clone());
            claimed.push((declared, operation, body.clone()));
        }
    }

    let mut schemas = serde_json::Map::new();
    for (declared, operation, mut body) in claimed {
        rewrite_refs(&mut body, &|name| {
            names
                .get(&(operation, name.to_string()))
                .cloned()
                .unwrap_or_else(|| name.to_string())
        });
        if let Some(previous) = schemas.get(&declared) {
            assert_eq!(
                previous, &body,
                "two different types are hoisted as `{declared}`"
            );
        }
        schemas.insert(declared, body);
    }
    (schemas, names)
}

/// Names reachable by `$ref` from `schema`, transitively.
fn referenced(
    schema: &Value,
    schemas: &serde_json::Map<String, Value>,
    seen: &mut BTreeSet<String>,
) {
    match schema {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix(COMPONENTS))
            {
                if seen.insert(name.to_string()) {
                    if let Some(target) = schemas.get(name) {
                        referenced(target, schemas, seen);
                    }
                }
            }
            for child in map.values() {
                referenced(child, schemas, seen);
            }
        }
        Value::Array(items) => {
            for item in items {
                referenced(item, schemas, seen);
            }
        }
        _ => {}
    }
}

/// Mark every property of an object schema required.
///
/// Only ever applied to a schema a *response* alone mentions. schemars derives
/// `required` from what deserialization accepts: a field with a serde default,
/// and every `Option`, may be omitted by a *sender*. Serialization is not
/// symmetric — serde writes every field of a struct, an absent `Option` as an
/// explicit `null` — so a response's `required` list under-reports by exactly
/// the fields the server always emits, and a generated client would make its
/// callers test for an absence that cannot happen.
///
/// The exception this cannot see is `#[serde(skip_serializing_if)]`, which does
/// omit a field. Fifteen DTO fields use it and are described here as present.
fn require_every_property(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if let Some(properties) = map.get("properties").and_then(Value::as_object) {
                let names: Vec<Value> = properties.keys().map(|name| json!(name)).collect();
                if !names.is_empty() {
                    map.insert("required".to_string(), Value::Array(names));
                }
            }
            for (key, child) in map.iter_mut() {
                // A `$ref` is a component, marked in its own right; `required`
                // and `enum` hold names and values, not schemas.
                if !matches!(
                    key.as_str(),
                    "$ref" | "required" | "enum" | "const" | "default"
                ) {
                    require_every_property(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                require_every_property(item);
            }
        }
        _ => {}
    }
}

/// Operands that travel in the query string instead of a JSON body.
///
/// `style: form` with `explode: true` over an object schema is OpenAPI's
/// spelling of `?name=value&other=value`, which is how the byte-encoded
/// operations — streams, websockets, downloads, uploads — carry their operands.
fn query_parameters(schema: Value) -> Value {
    json!([{
        "name": "operands",
        "in": "query",
        "required": true,
        "style": "form",
        "explode": true,
        "schema": schema,
    }])
}

/// Render the registry as an OpenAPI 3.1 document.
///
/// Routes are unique by construction, so this is a straight map over the
/// registry.
pub fn openapi_document(version: &str) -> Value {
    let (mut schemas, names) = shared_schemas();
    // A schema only a response mentions describes what the server writes, where
    // every field is present. One a request mentions keeps schemars' `required`,
    // because there a caller really may omit what it lists.
    let mut requestable = BTreeSet::new();
    for operation in operations() {
        let resolve = |name: &str| {
            names
                .get(&(operation.id, name.to_string()))
                .cloned()
                .unwrap_or_else(|| name.to_string())
        };
        let mut input = schema_body(&(operation.schema)());
        rewrite_refs(&mut input, &resolve);
        referenced(&input, &schemas, &mut requestable);
    }
    for (name, schema) in schemas.iter_mut() {
        if !requestable.contains(name) {
            require_every_property(schema);
        }
    }

    let mut paths = serde_json::Map::new();
    for operation in operations() {
        let resolve = |name: &str| {
            names
                .get(&(operation.id, name.to_string()))
                .cloned()
                .unwrap_or_else(|| name.to_string())
        };
        let mut input = schema_body(&(operation.schema)());
        rewrite_refs(&mut input, &resolve);
        let output = (operation.output_schema)();
        let output = match output.get("title").and_then(Value::as_str) {
            Some(title) if is_shared_name(title) => {
                json!({ "$ref": format!("{COMPONENTS}{}", resolve(title)) })
            }
            _ => {
                // An anonymous per-operation `Output` keeps its title: it is
                // the only name a generated client has to call the type by.
                let mut inline = match &output {
                    Value::Object(map) => Value::Object(
                        map.iter()
                            .filter(|(key, _)| !matches!(key.as_str(), "$schema" | "$defs"))
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                    ),
                    other => other.clone(),
                };
                rewrite_refs(&mut inline, &resolve);
                require_every_property(&mut inline);
                inline
            }
        };
        let mut definition = json!({
            "operationId": operation.id,
            "summary": operation.summary,
            "tags": [operation.bundle],
            "x-loom-actor": operation.actor.as_str(),
            "x-loom-scope": operation.scope.as_str(),
            "x-loom-risk": operation.risk.as_str(),
            "x-loom-io": operation.io.as_str(),
            "x-loom-grants": operation.grants,
            "responses": {
                "200": {
                    "description": "success",
                    "content": { "application/json": { "schema": output } },
                },
            },
        });
        if let Some(cli) = operation.cli {
            definition["x-loom-cli"] = json!(cli.invocation());
        }
        if !operation.context.is_empty() {
            // The request schema elides these: a session caller cannot supply
            // them. Every other caller has to, so the document has to say so.
            definition["x-loom-context"] = json!(operation
                .context
                .iter()
                .map(|field| field.name)
                .collect::<Vec<_>>());
        }
        match operation.io {
            Io::Json | Io::Session => {
                definition["requestBody"] = json!({
                    "required": true,
                    "content": { "application/json": { "schema": input } },
                });
            }
            Io::Upload => {
                definition["parameters"] = query_parameters(input);
                definition["requestBody"] = json!({
                    "required": true,
                    "content": {
                        "application/octet-stream": { "schema": { "type": "string", "format": "binary" } },
                    },
                });
            }
            Io::Stream | Io::Duplex | Io::Download => {
                definition["parameters"] = query_parameters(input);
            }
        }
        let method = operation.method().to_ascii_lowercase();
        paths
            .entry(operation.path())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("operation path object")
            .insert(method, definition);
    }
    json!({
        "openapi": "3.1.0",
        "info": { "title": "Loom API", "version": version },
        "paths": paths,
        "components": { "schemas": schemas },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_structurally_valid() {
        validate_operation_registry().unwrap();
    }

    #[test]
    fn every_operation_resolves_from_its_own_route() {
        for operation in operations() {
            let found = operation_for_request(operation.method(), &operation.path())
                .unwrap_or_else(|| panic!("{} does not resolve from its route", operation.id));
            assert_eq!(found.id, operation.id);
        }
    }
}
