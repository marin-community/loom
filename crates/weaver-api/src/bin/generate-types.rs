//! Write `frontend/src/api/generated.ts` from the registry's OpenAPI document.
//!
//! `cargo run -p weaver-api --bin generate-types`
//!
//! Run it whenever a DTO or an operation changes. The SPA builds with rspack
//! and never invokes cargo, so the rendered module is checked in.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

/// Where the rendered module lands in the frontend source tree.
const OUTPUT: &str = "../loom/frontend/src/api/generated.ts";

const BANNER: &str = "\
// Generated from Loom's OpenAPI document — do not edit. Every type below is
// derived from the same `OperationSpec` that answers `/api/openapi.json`.
//
// Regenerate: cargo run -p weaver-api --bin generate-types
";

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(OUTPUT);
    std::fs::create_dir_all(path.parent().expect("output directory")).expect("create dir");
    std::fs::write(&path, render()).expect("write generated module");
    println!("wrote {}", path.display());
}

// -- Type expressions --------------------------------------------------------

/// Render one JSON Schema as a TypeScript type expression.
///
/// Deliberately partial. Anything outside the constructs schemars actually
/// emits for Loom's DTOs becomes `unknown` rather than a guess: an unchecked
/// type is better than a confidently wrong one.
fn ts_type(schema: &Value, indent: usize) -> String {
    let map = match schema {
        // schemars writes `true` for a field that accepts any JSON.
        Value::Bool(true) => return "unknown".to_string(),
        Value::Bool(false) => return "never".to_string(),
        Value::Object(map) => map,
        _ => return "unknown".to_string(),
    };
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        return reference
            .rsplit('/')
            .next()
            .unwrap_or("unknown")
            .to_string();
    }
    if let Some(value) = map.get("const") {
        return literal(value);
    }
    if let Some(values) = map.get("enum").and_then(Value::as_array) {
        return union(values.iter().map(literal));
    }
    if let Some(variants) = map
        .get("anyOf")
        .or_else(|| map.get("oneOf"))
        .and_then(Value::as_array)
    {
        return union(variants.iter().map(|variant| ts_type(variant, indent)));
    }
    match map.get("type") {
        Some(Value::String(name)) => scalar(name, map, indent),
        Some(Value::Array(names)) => union(names.iter().map(|name| match name.as_str() {
            Some(name) => scalar(name, map, indent),
            None => "unknown".to_string(),
        })),
        // Neither a type nor a combinator: `serde_json::Value`, i.e. any JSON.
        _ => "unknown".to_string(),
    }
}

fn scalar(name: &str, map: &Map<String, Value>, indent: usize) -> String {
    match name {
        "string" => "string".to_string(),
        // JavaScript has one number type and the wire format is JSON, so the
        // Rust integer width `format` records is nothing a caller could act on.
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let item = match map.get("items") {
                Some(items) => ts_type(items, indent),
                None => "unknown".to_string(),
            };
            if item.contains('|') || item.contains('{') {
                format!("({item})[]")
            } else {
                format!("{item}[]")
            }
        }
        "object" => object_type(map, indent),
        _ => "unknown".to_string(),
    }
}

fn object_type(map: &Map<String, Value>, indent: usize) -> String {
    if map.contains_key("properties") {
        let mut out = String::from("{\n");
        out.push_str(&properties(map, indent + 1));
        out.push_str(&"  ".repeat(indent));
        out.push('}');
        return out;
    }
    match map.get("additionalProperties") {
        Some(Value::Bool(false)) => "Record<string, never>".to_string(),
        Some(additional) => format!("Record<string, {}>", ts_type(additional, indent)),
        None => "Record<string, unknown>".to_string(),
    }
}

fn union(parts: impl Iterator<Item = String>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for part in parts {
        if !seen.contains(&part) {
            seen.push(part);
        }
    }
    if seen.is_empty() {
        return "unknown".to_string();
    }
    seen.join(" | ")
}

fn literal(value: &Value) -> String {
    match value {
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

// -- Emission ----------------------------------------------------------------

/// Render a schema `description` as TSDoc.
fn doc(description: Option<&str>, indent: usize) -> String {
    let Some(description) = description else {
        return String::new();
    };
    let pad = "  ".repeat(indent);
    // The text is Rust doc comment prose; a `*/` in it would close the comment.
    let text = description.replace("*/", "*\\/");
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() == 1 {
        return format!("{pad}/** {} */\n", lines[0]);
    }
    let mut out = format!("{pad}/**\n");
    for line in lines {
        if line.is_empty() {
            out.push_str(&format!("{pad} *\n"));
        } else {
            out.push_str(&format!("{pad} * {line}\n"));
        }
    }
    out.push_str(&format!("{pad} */\n"));
    out
}

fn properties(map: &Map<String, Value>, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let required: BTreeSet<&str> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let empty = Map::new();
    let mut out = String::new();
    for (name, schema) in map
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty)
    {
        out.push_str(&doc(
            schema.get("description").and_then(Value::as_str),
            indent,
        ));
        let optional = if required.contains(name.as_str()) {
            ""
        } else {
            "?"
        };
        out.push_str(&format!(
            "{pad}{name}{optional}: {};\n",
            ts_type(schema, indent)
        ));
    }
    out
}

/// A declaration for one `components/schemas` entry.
fn declaration(name: &str, schema: &Value) -> String {
    let mut out = doc(schema.get("description").and_then(Value::as_str), 0);
    let is_object = schema.get("type").and_then(Value::as_str) == Some("object")
        && schema.get("properties").is_some();
    if is_object {
        let map = schema.as_object().expect("object schema");
        out.push_str(&format!("export interface {name} {{\n"));
        out.push_str(&properties(map, 1));
        out.push_str("}\n");
    } else {
        out.push_str(&format!("export type {name} = {};\n", ts_type(schema, 0)));
    }
    out
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

/// One operation, read out of the document.
struct Operation {
    id: String,
    method: String,
    path: String,
    summary: String,
    /// The operand schema, whether it arrives as a JSON body or a query string.
    operands: Value,
    /// Fields the request schema elides because the dispatcher fills them.
    context: Vec<String>,
    response: Value,
}

fn read_operations(document: &Value) -> Vec<Operation> {
    let empty = Map::new();
    let mut operations = Vec::new();
    for (path, methods) in document["paths"].as_object().unwrap_or(&empty) {
        for (method, definition) in methods.as_object().unwrap_or(&empty) {
            let operands = definition
                .pointer("/requestBody/content/application~1json/schema")
                .or_else(|| definition.pointer("/parameters/0/schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
            operations.push(Operation {
                id: definition["operationId"]
                    .as_str()
                    .expect("operationId")
                    .to_string(),
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                summary: definition["summary"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                operands,
                context: definition["x-loom-context"]
                    .as_array()
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                response: definition
                    .pointer("/responses/200/content/application~1json/schema")
                    .cloned()
                    .unwrap_or(Value::Null),
            });
        }
    }
    operations.sort_by(|left, right| left.id.cmp(&right.id));
    operations
}

/// The caller-facing input type for one operation.
///
/// The request schema elides the fields the dispatcher fills from session
/// context, because a session caller cannot supply them. The SPA is a `User`
/// caller with no session of its own, so it must: `x-loom-context` names them
/// and they come back as optional strings, which is what "fill only what the
/// caller left unset" means for a caller with no context to fill from.
fn input_declaration(name: &str, operation: &Operation) -> String {
    let map = operation.operands.as_object().cloned().unwrap_or_default();
    let mut out = format!("export interface {name} {{\n");
    out.push_str(&properties(&map, 1));
    for field in &operation.context {
        out.push_str(&doc(
            Some("Supplied by the dispatcher from the caller's session context when omitted."),
            1,
        ));
        out.push_str(&format!("  {field}?: string;\n"));
    }
    out.push_str("}\n");
    out
}

fn render() -> String {
    let document = weaver_api::operations::openapi_document(env!("CARGO_PKG_VERSION"));
    let operations = read_operations(&document);

    let mut out = String::from(BANNER);

    out.push_str(
        "\n// -- Shared types ---------------------------------------------------------\n",
    );
    let empty = Map::new();
    let schemas = document
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    // The document's map is insertion-ordered; sort so the file reads as a
    // dictionary and a new type lands next to its neighbours in the diff.
    for (name, schema) in schemas.iter().collect::<BTreeMap<_, _>>() {
        out.push('\n');
        out.push_str(&declaration(name, schema));
    }

    out.push_str(
        "\n// -- Per-operation input and output ---------------------------------------\n",
    );
    // Declared name -> the operation that claimed it, so two operations
    // generating one name is caught here rather than by `tsc`.
    let mut declared: BTreeMap<String, &str> = BTreeMap::new();
    // Operation id -> its input type name and its output type expression.
    let mut bindings: Vec<(&str, String, String)> = Vec::new();
    for operation in &operations {
        let input = format!("{}Input", pascal(&operation.id));
        if let Some(previous) = declared.insert(input.clone(), &operation.id) {
            panic!(
                "`{input}` is generated for both {previous} and {}",
                operation.id
            );
        }
        out.push('\n');
        out.push_str(&doc(Some(&operation.summary), 0));
        out.push_str(&input_declaration(&input, operation));

        let title = operation.response["title"].as_str().unwrap_or_default();
        let output = if title == "Output" {
            // The operation's Rust `Output` is an anonymous per-operation
            // struct, so it is named after the operation.
            let name = format!("{}Output", pascal(&operation.id));
            if let Some(previous) = declared.insert(name.clone(), &operation.id) {
                panic!(
                    "`{name}` is generated for both {previous} and {}",
                    operation.id
                );
            }
            let map = operation.response.as_object().cloned().unwrap_or_default();
            out.push('\n');
            out.push_str(&doc(operation.response["description"].as_str(), 0));
            out.push_str(&format!("export interface {name} {{\n"));
            out.push_str(&properties(&map, 1));
            out.push_str("}\n");
            name
        } else {
            ts_type(&operation.response, 0)
        };
        bindings.push((&operation.id, input, output));
    }

    out.push_str(
        "\n// -- The operation table --------------------------------------------------\n\n\
         /** Every registered operation, keyed by its identity. An id this map does not\n\
         \x20* carry is a compile error, not a 404. */\n\
         export interface Operations {\n",
    );
    for ((id, input, output), operation) in bindings.iter().zip(&operations) {
        out.push_str(&doc(Some(&operation.summary), 1));
        out.push_str(&format!(
            "  '{id}': {{ input: {input}; output: {output} }};\n"
        ));
    }
    out.push_str("}\n\n");

    out.push_str(
        "export type OperationId = keyof Operations;\n\
         export type OperationInput<K extends OperationId> = Operations[K]['input'];\n\
         export type OperationOutput<K extends OperationId> = Operations[K]['output'];\n\n\
         /** Each operation's canonical route, derived in Rust from its identity by\n\
         \x20* `OperationSpec::path`. The frontend reads it rather than deriving it a\n\
         \x20* second time. */\n\
         export const OPERATION_ROUTES = {\n",
    );
    for operation in &operations {
        out.push_str(&format!(
            "  '{}': {{ method: '{}', path: '{}' }},\n",
            operation.id, operation.method, operation.path
        ));
    }
    out.push_str("} as const satisfies Record<OperationId, { method: string; path: string }>;\n");
    out
}
