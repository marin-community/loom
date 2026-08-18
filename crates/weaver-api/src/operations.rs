//! Transport-neutral registry for Loom's agent-facing operations.
//!
//! REST is the execution boundary.  This registry gives every operation a
//! stable identity and records how the CLI and MCP project it, so discovery,
//! help, capability policy, and adapters can join on one key instead of
//! maintaining unrelated command catalogues.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorPolicy {
    SessionSelf,
    User,
    Admin,
    Internal,
}

impl ActorPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionSelf => "session_self",
            Self::User => "user",
            Self::Admin => "admin",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationRisk {
    Read,
    Write,
    Destructive,
    ExternalWrite,
}

/// The durable resource boundary an operation is evaluated against.
///
/// Actor answers *who* may call an operation; scope answers *which instance*
/// that actor may reach. Keeping both axes in the registry prevents adapters
/// from treating a branch-owned artifact like an unconstrained session call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationScope {
    Session,
    Branch,
    Repository,
}

impl OperationScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Branch => "branch",
            Self::Repository => "repository",
        }
    }
}

impl OperationRisk {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
            Self::ExternalWrite => "external_write",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct McpProjection {
    pub server: &'static str,
    pub tool: &'static str,
    /// Preserve transport-specific wording when it is already part of a pinned
    /// MCP capability digest. New verbs normally inherit `OperationSpec::summary`.
    pub description: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentKind {
    String,
    Integer,
    Boolean,
    StringList,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentDefault {
    String(&'static str),
    Integer(i64),
    Boolean(bool),
}

/// Transport-neutral metadata for the routine argument shapes Loom can expose
/// through discovery and MCP without repeating hand-written JSON Schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentSpec {
    pub name: &'static str,
    pub kind: ArgumentKind,
    pub required: bool,
    pub description: Option<&'static str>,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    pub pattern: Option<&'static str>,
    pub choices: &'static [&'static str],
    pub default: Option<ArgumentDefault>,
    pub unique_items: bool,
}

impl ArgumentSpec {
    const fn new(name: &'static str, kind: ArgumentKind) -> Self {
        Self {
            name,
            kind,
            required: false,
            description: None,
            minimum: None,
            maximum: None,
            pattern: None,
            choices: &[],
            default: None,
            unique_items: false,
        }
    }

    pub const fn string(name: &'static str) -> Self {
        Self::new(name, ArgumentKind::String)
    }

    pub const fn integer(name: &'static str) -> Self {
        Self::new(name, ArgumentKind::Integer)
    }

    pub const fn boolean(name: &'static str) -> Self {
        Self::new(name, ArgumentKind::Boolean)
    }

    pub const fn string_list(name: &'static str) -> Self {
        Self::new(name, ArgumentKind::StringList)
    }

    pub const fn any(name: &'static str) -> Self {
        Self::new(name, ArgumentKind::Any)
    }

    pub const fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub const fn description(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }

    /// String length or integer value lower bound, depending on `kind`.
    pub const fn minimum(mut self, minimum: i64) -> Self {
        self.minimum = Some(minimum);
        self
    }

    /// String length or integer value upper bound, depending on `kind`.
    pub const fn maximum(mut self, maximum: i64) -> Self {
        self.maximum = Some(maximum);
        self
    }

    pub const fn pattern(mut self, pattern: &'static str) -> Self {
        self.pattern = Some(pattern);
        self
    }

    pub const fn choices(mut self, choices: &'static [&'static str]) -> Self {
        self.choices = choices;
        self
    }

    pub const fn default_string(mut self, value: &'static str) -> Self {
        self.default = Some(ArgumentDefault::String(value));
        self
    }

    pub const fn default_integer(mut self, value: i64) -> Self {
        self.default = Some(ArgumentDefault::Integer(value));
        self
    }

    pub const fn default_boolean(mut self, value: bool) -> Self {
        self.default = Some(ArgumentDefault::Boolean(value));
        self
    }

    pub const fn unique_items(mut self) -> Self {
        self.unique_items = true;
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OperationSpec {
    pub id: &'static str,
    pub bundle: &'static str,
    pub summary: &'static str,
    pub actor: ActorPolicy,
    pub scope: OperationScope,
    pub risk: OperationRisk,
    pub method: &'static str,
    pub path: &'static str,
    pub cli: Option<&'static str>,
    pub mcp: Option<McpProjection>,
    pub args: &'static [ArgumentSpec],
    pub capabilities: &'static [&'static str],
}

/// Exact REST request emitted by a typed API operation.
///
/// Operation declarations generate these request builders alongside their
/// discoverable metadata. Keeping the builder typed avoids a reflective
/// `Value` walker while still giving [`crate::Client`] one generic invocation
/// path.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationRequest {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}

impl OperationRequest {
    pub fn without_body(operation: &'static OperationSpec, path: impl Into<String>) -> Self {
        Self {
            method: operation.method,
            path: path.into(),
            body: None,
        }
    }

    pub fn json<T: Serialize>(
        operation: &'static OperationSpec,
        path: impl Into<String>,
        body: &T,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            method: operation.method,
            path: path.into(),
            body: Some(serde_json::to_value(body)?),
        })
    }
}

pub(crate) fn encode_path_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A compile-time API request/response contract bound to one registered
/// operation identity.
///
/// Server handlers bind to this identity in `loom`; remote callers execute it
/// through [`crate::Client::invoke`]. Concrete application code stays typed —
/// JSON erasure is confined to the generated REST request and response edges.
pub trait ApiOperation: Send + Sync + 'static {
    type Input: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Output: Serialize + DeserializeOwned + Send + Sync + 'static;

    const SPEC: &'static OperationSpec;

    fn request(input: &Self::Input) -> anyhow::Result<OperationRequest>;
}

/// One first-party resource bundle projected across Loom's transports.
///
/// Factories keep registration explicit and deterministic while allowing each
/// resource module to own its operation declarations. Transport crates join
/// their API, CLI, and MCP factories to this stable bundle name.
#[derive(Debug, Clone, Copy)]
pub struct OperationBundle {
    pub name: &'static str,
    pub label: &'static str,
    pub operations: &'static [OperationSpec],
}

pub type OperationBundleFactory = fn() -> OperationBundle;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpProjectionView {
    pub server: String,
    pub tool: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArgumentView {
    pub name: String,
    pub kind: ArgumentKind,
    pub required: bool,
    pub description: Option<String>,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    pub pattern: Option<String>,
    pub choices: Vec<String>,
    pub default: Option<Value>,
    pub unique_items: bool,
}

impl From<&ArgumentSpec> for ArgumentView {
    fn from(spec: &ArgumentSpec) -> Self {
        Self {
            name: spec.name.to_string(),
            kind: spec.kind,
            required: spec.required,
            description: spec.description.map(str::to_string),
            minimum: spec.minimum,
            maximum: spec.maximum,
            pattern: spec.pattern.map(str::to_string),
            choices: spec
                .choices
                .iter()
                .map(|choice| (*choice).to_string())
                .collect(),
            default: spec.default.map(argument_default_value),
            unique_items: spec.unique_items,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationView {
    pub id: String,
    pub bundle: String,
    pub summary: String,
    pub actor: ActorPolicy,
    pub scope: OperationScope,
    pub risk: OperationRisk,
    pub method: String,
    pub path: String,
    pub cli: Option<String>,
    pub mcp: Option<McpProjectionView>,
    pub args: Vec<ArgumentView>,
    pub capabilities: Vec<String>,
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
            method: spec.method.to_string(),
            path: spec.path.to_string(),
            cli: spec.cli.map(str::to_string),
            mcp: spec.mcp.map(|projection| McpProjectionView {
                server: projection.server.to_string(),
                tool: projection.tool.to_string(),
                description: projection.description.map(str::to_string),
            }),
            args: spec.args.iter().map(ArgumentView::from).collect(),
            capabilities: spec
                .capabilities
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiMetaView {
    pub product: String,
    pub version: String,
    pub operation_registry_version: u32,
    pub operations_url: String,
    pub openapi_url: String,
}

const NO_ARGS: &[ArgumentSpec] = &[];

macro_rules! operation {
    ($id:literal, $bundle:literal, $summary:literal, $actor:ident, $risk:ident,
     $method:literal, $path:literal, $cli:expr, $mcp:expr, $capabilities:expr $(, $args:expr)?) => {
        OperationSpec {
            id: $id,
            bundle: $bundle,
            summary: $summary,
            actor: ActorPolicy::$actor,
            scope: OperationScope::Session,
            risk: OperationRisk::$risk,
            method: $method,
            path: $path,
            cli: $cli,
            mcp: $mcp,
            args: operation!(@args $($args)?),
            capabilities: $capabilities,
        }
    };
    (@args) => { NO_ARGS };
    (@args $args:expr) => { $args };
}

macro_rules! branch_operation {
    ($id:literal, $bundle:literal, $summary:literal, $actor:ident, $risk:ident,
     $method:literal, $path:literal, $cli:expr, $mcp:expr, $capabilities:expr $(, $args:expr)?) => {{
        let mut operation = operation!(
            $id,
            $bundle,
            $summary,
            $actor,
            $risk,
            $method,
            $path,
            $cli,
            $mcp,
            $capabilities
            $(, $args)?
        );
        operation.scope = OperationScope::Branch;
        operation
    }};
}

macro_rules! repository_operation {
    ($id:literal, $bundle:literal, $summary:literal, $actor:ident, $risk:ident,
     $method:literal, $path:literal, $cli:expr, $mcp:expr, $capabilities:expr $(, $args:expr)?) => {{
        let mut operation = operation!(
            $id,
            $bundle,
            $summary,
            $actor,
            $risk,
            $method,
            $path,
            $cli,
            $mcp,
            $capabilities
            $(, $args)?
        );
        operation.scope = OperationScope::Repository;
        operation
    }};
}

/// Bind one addressable descriptor to concrete request/response types and its
/// exact generated REST encoder. Resource modules keep these declarations next
/// to the argument and authority metadata that defines the public operation.
macro_rules! typed_api_operation {
    ($name:ident, $spec:ident, $input:ty, $output:ty, $request:expr) => {
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl ApiOperation for $name {
            type Input = $input;
            type Output = $output;

            const SPEC: &'static OperationSpec = &$spec;

            fn request(input: &Self::Input) -> anyhow::Result<OperationRequest> {
                ($request)(input)
            }
        }
    };
}

const fn mcp(server: &'static str, tool: &'static str) -> Option<McpProjection> {
    Some(McpProjection {
        server,
        tool,
        description: None,
    })
}

const fn described_mcp(
    server: &'static str,
    tool: &'static str,
    description: &'static str,
) -> Option<McpProjection> {
    Some(McpProjection {
        server,
        tool,
        description: Some(description),
    })
}

mod artifacts;
mod channels;
pub mod issues;
pub mod permissions;
mod sessions;
pub static OPERATION_BUNDLE_FACTORIES: &[OperationBundleFactory] = &[
    sessions::bundle,
    channels::bundle,
    artifacts::bundle,
    issues::bundle,
    permissions::bundle,
];

pub fn operation_bundles() -> impl Iterator<Item = OperationBundle> {
    OPERATION_BUNDLE_FACTORIES.iter().map(|factory| factory())
}

pub fn operations() -> impl Iterator<Item = &'static OperationSpec> {
    operation_bundles().flat_map(|bundle| bundle.operations.iter())
}

fn argument_default_value(default: ArgumentDefault) -> Value {
    match default {
        ArgumentDefault::String(value) => Value::String(value.to_string()),
        ArgumentDefault::Integer(value) => Value::Number(value.into()),
        ArgumentDefault::Boolean(value) => Value::Bool(value),
    }
}

fn argument_schema(argument: &ArgumentSpec) -> Value {
    let mut schema = serde_json::Map::new();
    match argument.kind {
        ArgumentKind::String => {
            schema.insert("type".to_string(), json!("string"));
            if let Some(minimum) = argument.minimum {
                schema.insert("minLength".to_string(), json!(minimum));
            }
            if let Some(maximum) = argument.maximum {
                schema.insert("maxLength".to_string(), json!(maximum));
            }
            if let Some(pattern) = argument.pattern {
                schema.insert("pattern".to_string(), json!(pattern));
            }
            if !argument.choices.is_empty() {
                schema.insert("enum".to_string(), json!(argument.choices));
            }
        }
        ArgumentKind::Integer => {
            schema.insert("type".to_string(), json!("integer"));
            if let Some(minimum) = argument.minimum {
                schema.insert("minimum".to_string(), json!(minimum));
            }
            if let Some(maximum) = argument.maximum {
                schema.insert("maximum".to_string(), json!(maximum));
            }
        }
        ArgumentKind::Boolean => {
            schema.insert("type".to_string(), json!("boolean"));
        }
        ArgumentKind::StringList => {
            schema.insert("type".to_string(), json!("array"));
            let mut items = serde_json::Map::new();
            items.insert("type".to_string(), json!("string"));
            if !argument.choices.is_empty() {
                items.insert("enum".to_string(), json!(argument.choices));
            }
            schema.insert("items".to_string(), Value::Object(items));
            if argument.unique_items {
                schema.insert("uniqueItems".to_string(), Value::Bool(true));
            }
        }
        ArgumentKind::Any => {}
    }
    if let Some(description) = argument.description {
        schema.insert("description".to_string(), json!(description));
    }
    if let Some(default) = argument.default {
        schema.insert("default".to_string(), argument_default_value(default));
    }
    Value::Object(schema)
}

pub fn operation_input_schema(operation: &OperationSpec) -> Value {
    let properties = operation
        .args
        .iter()
        .map(|argument| (argument.name.to_string(), argument_schema(argument)))
        .collect::<serde_json::Map<_, _>>();
    let required = operation
        .args
        .iter()
        .filter(|argument| argument.required)
        .map(|argument| argument.name)
        .collect::<Vec<_>>();
    let mut schema = serde_json::Map::from_iter([
        ("type".to_string(), json!("object")),
        ("additionalProperties".to_string(), Value::Bool(false)),
        ("properties".to_string(), Value::Object(properties)),
    ]);
    if !required.is_empty() {
        schema.insert("required".to_string(), json!(required));
    }
    Value::Object(schema)
}

/// Generate the routine MCP catalogue for one built-in server. Runtime
/// adapters retain only execution callbacks and any explicitly custom tools.
pub fn mcp_tools(server: &str) -> Value {
    Value::Array(
        operations()
            .filter_map(|operation| {
                let projection = operation.mcp.filter(|mcp| mcp.server == server)?;
                Some(json!({
                    "name": projection.tool,
                    "description": projection.description.unwrap_or(operation.summary),
                    "inputSchema": operation_input_schema(operation),
                }))
            })
            .collect(),
    )
}

/// Generate one server's routine MCP catalogue in its established advertised
/// order. Ordering is observable to clients even though capability identity is
/// keyed by tool name, so adapters preserve it during the registry migration.
pub fn mcp_tools_ordered(server: &str, tool_order: &[&str]) -> Value {
    let by_name = mcp_tools(server)
        .as_array()
        .expect("generated MCP catalogue is an array")
        .iter()
        .map(|tool| {
            (
                tool["name"]
                    .as_str()
                    .expect("generated MCP tool has a name")
                    .to_string(),
                tool.clone(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    Value::Array(
        tool_order
            .iter()
            .map(|name| {
                by_name
                    .get(*name)
                    .unwrap_or_else(|| panic!("unregistered MCP tool {server}::{name}"))
                    .clone()
            })
            .collect(),
    )
}

/// Validate the compile-time bundle factories before a transport mounts them.
/// Transport registries add their own projection checks on top of these
/// neutral identity, ownership, and authority invariants.
pub fn validate_operation_registry() -> Result<(), String> {
    let mut bundle_names = std::collections::BTreeSet::new();
    let mut operation_ids = std::collections::BTreeSet::new();
    let mut cli_paths = std::collections::BTreeSet::new();
    let mut mcp_tools = std::collections::BTreeSet::new();
    let mut registered = Vec::new();
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
                    "operation {} declares bundle {} but factory registered it under {}",
                    operation.id, operation.bundle, bundle.name
                ));
            }
            if !operation_ids.insert(operation.id) {
                return Err(format!("duplicate operation id {}", operation.id));
            }
            if let Some(cli) = operation.cli {
                if !cli_paths.insert(cli) {
                    return Err(format!("duplicate CLI projection {cli}"));
                }
            }
            if let Some(mcp) = operation.mcp {
                if !mcp_tools.insert((mcp.server, mcp.tool)) {
                    return Err(format!(
                        "duplicate MCP projection {}::{}",
                        mcp.server, mcp.tool
                    ));
                }
            }
            let mut argument_names = std::collections::BTreeSet::new();
            for argument in operation.args {
                if argument.name.is_empty() {
                    return Err(format!(
                        "operation {} has an empty argument name",
                        operation.id
                    ));
                }
                if !argument_names.insert(argument.name) {
                    return Err(format!(
                        "operation {} has duplicate argument {}",
                        operation.id, argument.name
                    ));
                }
                if argument
                    .minimum
                    .zip(argument.maximum)
                    .is_some_and(|(min, max)| min > max)
                {
                    return Err(format!(
                        "operation {} argument {} has an inverted range",
                        operation.id, argument.name
                    ));
                }
                if (argument.pattern.is_some() && argument.kind != ArgumentKind::String)
                    || (!argument.choices.is_empty()
                        && !matches!(
                            argument.kind,
                            ArgumentKind::String | ArgumentKind::StringList
                        ))
                    || (argument.unique_items && argument.kind != ArgumentKind::StringList)
                    || ((argument.minimum.is_some() || argument.maximum.is_some())
                        && !matches!(argument.kind, ArgumentKind::String | ArgumentKind::Integer))
                {
                    return Err(format!(
                        "operation {} argument {} uses constraints unsupported by {:?}",
                        operation.id, argument.name, argument.kind
                    ));
                }
                let default_matches = matches!(
                    (argument.kind, argument.default),
                    (_, None)
                        | (ArgumentKind::String, Some(ArgumentDefault::String(_)))
                        | (ArgumentKind::Integer, Some(ArgumentDefault::Integer(_)))
                        | (ArgumentKind::Boolean, Some(ArgumentDefault::Boolean(_)))
                );
                if !default_matches {
                    return Err(format!(
                        "operation {} argument {} has a default of another kind",
                        operation.id, argument.name
                    ));
                }
            }
            if operation.risk != OperationRisk::Read && operation.capabilities.is_empty() {
                return Err(format!(
                    "mutating operation {} has no capability boundary",
                    operation.id
                ));
            }
            registered.push(operation);
        }
    }
    for (index, operation) in registered.iter().enumerate() {
        for other in &registered[index + 1..] {
            if operation.method == other.method
                && operation.path == other.path
                && (operation.actor != other.actor
                    || operation.scope != other.scope
                    || operation.capabilities != other.capabilities)
            {
                return Err(format!(
                    "operations {} and {} share a route but not its authority boundary",
                    operation.id, other.id
                ));
            }
        }
    }
    Ok(())
}

/// Ensure a transport has at least one factory for every operation bundle and
/// does not reference a bundle outside the neutral registry. Transports that
/// require exactly one factory per bundle add their own multiplicity check.
pub fn validate_operation_bundle_coverage<'a>(
    transport: &str,
    registered_bundles: impl IntoIterator<Item = &'a str>,
) -> Result<(), String> {
    validate_operation_registry()?;
    let expected = operation_bundles()
        .map(|bundle| bundle.name)
        .collect::<std::collections::BTreeSet<_>>();
    let registered = registered_bundles
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if registered != expected {
        return Err(format!(
            "{transport} bundle factories must cover {expected:?}, registered {registered:?}"
        ));
    }
    Ok(())
}

pub fn operation(id: &str) -> Option<&'static OperationSpec> {
    operations().find(|operation| operation.id == id)
}

fn path_matches(template: &str, path: &str) -> bool {
    let template = template.trim_matches('/').split('/');
    let path = path.trim_matches('/').split('/');
    let mut template = template.peekable();
    let mut path = path.peekable();
    loop {
        match (template.next(), path.next()) {
            (None, None) => return true,
            (Some(expected), Some(actual))
                if (expected.starts_with('{') && expected.ends_with('}')) || expected == actual => {
            }
            _ => return false,
        }
    }
}

/// Resolve a REST request to its transport-neutral operation. Multiple
/// semantic operations may intentionally share one method/path (for example a
/// decision body choosing approve or deny); those rows must carry the same
/// actor and capability boundary, and the first is sufficient for admission.
pub fn operation_for_request(method: &str, path: &str) -> Option<&'static OperationSpec> {
    let method = method.to_ascii_uppercase();
    let path = if path.starts_with("/api/") || path == "/api" {
        path
    } else {
        // Middleware sometimes works with the nested router's stripped path.
        // Normalize it to the public API path recorded in the registry.
        return operations().find(|operation| {
            operation.method == method
                && operation
                    .path
                    .strip_prefix("/api")
                    .is_some_and(|template| path_matches(template, path))
        });
    };
    operations().find(|operation| operation.method == method && path_matches(operation.path, path))
}

/// Capabilities stamped into unrestricted session credentials. Legacy tokens
/// deserialize without a capability list and retain their historical policy;
/// newly minted credentials always carry this explicit set.
pub fn all_session_capabilities() -> Vec<String> {
    let mut capabilities = std::collections::BTreeSet::new();
    for operation in operations().filter(|operation| operation.actor == ActorPolicy::SessionSelf) {
        capabilities.extend(
            operation
                .capabilities
                .iter()
                .map(|capability| (*capability).to_string()),
        );
    }
    capabilities.into_iter().collect()
}

/// Translate an immutable MCP policy snapshot into the REST capabilities that
/// enforce the same operation boundary. Old `mcp/*` identities remain aliases
/// so a pinned profile does not lose API access during the namespace migration.
pub fn session_capabilities_from_mcp<'a>(
    restricted: bool,
    capability_sets: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    if !restricted {
        return all_session_capabilities();
    }

    let mut capabilities = std::collections::BTreeSet::from([
        "loom/sessions/read@v1".to_string(),
        "loom/permissions/read@v1".to_string(),
        "loom/permissions/request@v1".to_string(),
    ]);
    for capability in capability_sets {
        let canonical = match capability {
            "mcp/context/read@v1" | "mcp/history/self@v1" => Some("loom/sessions/read@v1"),
            "mcp/session/read@v1" => Some("loom/sessions/read@v1"),
            "mcp/session/status@v1" | "mcp/messaging/status@v1" => Some("loom/sessions/write@v1"),
            "mcp/channel/read@v1" => Some("loom/channels/read@v1"),
            "mcp/channel/write@v1" => Some("loom/channels/write@v1"),
            "mcp/artifact/read@v1" => Some("loom/artifacts/read@v1"),
            "mcp/artifact/write@v1" => Some("loom/artifacts/write@v1"),
            "mcp/github/comment@v1" | "loom/github/comment@v1" => Some("loom/github/use@v1"),
            value if value.starts_with("loom/") => Some(value),
            _ => None,
        };
        if let Some(canonical) = canonical {
            capabilities.insert(canonical.to_string());
        }
    }
    capabilities.into_iter().collect()
}

pub fn operation_views() -> Vec<OperationView> {
    operations().map(OperationView::from).collect()
}

pub fn operations_for_bundle(bundle: &str) -> impl Iterator<Item = &'static OperationSpec> + '_ {
    operation_bundles()
        .filter(move |registered| registered.name == bundle)
        .flat_map(|registered| registered.operations.iter())
}

/// A small OpenAPI discovery document. DTO schemas remain the Rust wire types;
/// operation ids and transport metadata are generated here rather than copied
/// into prose route tables.
pub fn openapi_document(version: &str) -> Value {
    let mut paths = serde_json::Map::new();
    for operation in operations() {
        let path = paths
            .entry(operation.path.to_string())
            .or_insert_with(|| json!({}));
        let methods = path.as_object_mut().expect("operation path object");
        let method = operation.method.to_ascii_lowercase();
        let definition = json!({
            "operationId": operation.id,
            "summary": operation.summary,
            "tags": [operation.bundle],
            "x-loom-actor": operation.actor.as_str(),
            "x-loom-scope": operation.scope.as_str(),
            "x-loom-risk": operation.risk.as_str(),
            "x-loom-cli": operation.cli,
            "x-loom-capabilities": operation.capabilities,
            "x-loom-operation-ids": [operation.id],
        });
        if let Some(existing) = methods.get_mut(&method) {
            existing["x-loom-operation-ids"]
                .as_array_mut()
                .expect("operation id extension is an array")
                .push(json!(operation.id));
        } else {
            methods.insert(method, definition);
        }
    }
    json!({
        "openapi": "3.1.0",
        "info": { "title": "Loom API", "version": version },
        "paths": paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_and_cli_projections_are_unique() {
        validate_operation_registry().unwrap();
    }

    #[test]
    fn ordinary_mcp_schema_is_generated_from_the_operation_arguments() {
        let issue_tools = mcp_tools("loom_issue");
        let list = issue_tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "list")
            .unwrap();
        assert_eq!(
            list,
            &json!({
                "name": "list",
                "description": "List work items in this session's repository.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "all": { "type": "boolean", "default": false }
                    }
                }
            })
        );
        let view = OperationView::from(operation("issues.list").unwrap());
        assert_eq!(view.args.len(), 1);
        assert_eq!(view.args[0].name, "all");
        assert_eq!(view.args[0].default, Some(Value::Bool(false)));
    }

    #[test]
    fn semantic_operations_sharing_a_route_share_authority() {
        let operations = operations().collect::<Vec<_>>();
        for (index, operation) in operations.iter().enumerate() {
            for other in &operations[index + 1..] {
                if operation.method == other.method && operation.path == other.path {
                    assert_eq!(operation.actor, other.actor, "shared actor boundary");
                    assert_eq!(operation.scope, other.scope, "shared scope boundary");
                    assert_eq!(
                        operation.capabilities, other.capabilities,
                        "shared capability boundary"
                    );
                }
            }
        }
    }

    #[test]
    fn permission_request_is_session_safe_but_decisions_are_human() {
        assert_eq!(
            operation("permissions.requests.create").unwrap().actor,
            ActorPolicy::SessionSelf
        );
        assert_eq!(
            operation("permissions.requests.approve").unwrap().actor,
            ActorPolicy::User
        );
    }

    #[test]
    fn request_matching_preserves_actor_and_scope_boundaries() {
        let artifact = operation_for_request(
            "POST",
            "/api/branches/branch-1/artifacts/design/threads/8/resolve",
        )
        .unwrap();
        assert_eq!(artifact.id, "artifacts.threads.resolve");
        assert_eq!(artifact.scope, OperationScope::Branch);

        let decision =
            operation_for_request("POST", "/api/permission-requests/request-1/decision").unwrap();
        assert_eq!(decision.actor, ActorPolicy::User);
    }

    #[test]
    fn openapi_keeps_semantic_aliases_for_shared_routes() {
        let document = openapi_document("test");
        let ids = document["paths"]["/api/permission-requests/{request}/decision"]["post"]
            ["x-loom-operation-ids"]
            .as_array()
            .unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn legacy_mcp_sets_map_to_the_same_restricted_rest_boundary() {
        let capabilities =
            session_capabilities_from_mcp(true, ["mcp/artifact/read@v1", "mcp/channel/write@v1"]);
        assert!(capabilities.contains(&"loom/artifacts/read@v1".to_string()));
        assert!(capabilities.contains(&"loom/channels/write@v1".to_string()));
        assert!(!capabilities.contains(&"loom/artifacts/write@v1".to_string()));
    }
}
