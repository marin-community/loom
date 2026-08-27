//! The transport-neutral operation contract.
//!
//! One operation is one declaration. This module holds the vocabulary that
//! declaration is written in and the traits the derives target; REST, CLI, and
//! MCP projections read every fact they need from here.
//!
//! `OperationSpec` derives its route, argument list, and command string from
//! the operation's own `Input` type through function pointers, so there is no
//! hand-maintained copy to drift from the declaration.

use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

/// Who may call an operation.
///
/// An operator-only action is declared `Admin`, never left out of the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActorPolicy {
    SessionSelf,
    /// The session credential itself, and nothing else — not even a human.
    ///
    /// Unlike `SessionSelf`, an operator cannot stand in: that would let one
    /// user obtain another's session token. `permissions.github.token` is why
    /// this exists.
    SessionOnly,
    User,
    Admin,
    Internal,
    /// Reachable with no credential at all.
    ///
    /// `auth.login` needs this: it must run before a principal exists.
    Anonymous,
}

impl ActorPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionSelf => "session_self",
            Self::SessionOnly => "session_only",
            Self::User => "user",
            Self::Admin => "admin",
            Self::Internal => "internal",
            Self::Anonymous => "anonymous",
        }
    }

    /// Whether an agent may reach this operation at all.
    ///
    /// Gates MCP projection, so a human-only operation — e.g. approving one's
    /// own permission request — has none.
    pub const fn agent_reachable(self) -> bool {
        matches!(self, Self::SessionSelf | Self::SessionOnly)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationRisk {
    Read,
    Write,
    Destructive,
    ExternalWrite,
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

/// The durable resource an operation is authorized against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationScope {
    Session,
    Branch,
    /// One channel, named by the operation's `channel` operand.
    ///
    /// Distinct from `Branch`: a channel is reachable from more than one branch
    /// (a session subscribes across its tree), so comparing the caller's own
    /// branch would let any channel id through.
    Channel,
    Repository,
    /// Not scoped to one resource — fleet-wide reads and administration.
    Global,
}

impl OperationScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Branch => "branch",
            Self::Channel => "channel",
            Self::Repository => "repository",
            Self::Global => "global",
        }
    }
}

/// How an operation's response is encoded.
///
/// The only axis on which a registered operation may be special: streaming
/// and upload endpoints still keep their descriptor, typed input, and
/// authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Io {
    /// JSON request, JSON response. The overwhelming majority.
    Json,
    /// Server-sent events. The CLI projects `--follow`; MCP normally abstains.
    Stream,
    /// Bidirectional websocket (terminals, the IDE proxy).
    Duplex,
    /// Raw request body: multipart or octet-stream.
    ///
    /// Describes the wire encoding, not the CLI interface — a JSON string
    /// stays `Json` even when the CLI takes a file for it (`from_file`).
    /// Non-JSON operations cannot expose MCP tools.
    Upload,
    /// Raw response body: bytes plus a guessed content type.
    ///
    /// For browser fetches through `<img src>` or download links. Operands
    /// arrive in the query string.
    Download,
    /// JSON body plus a browser session-cookie effect.
    ///
    /// Login and logout respond with `Set-Cookie` for HttpOnly sessions; a
    /// custom handler carries it since the generic dispatcher cannot emit it.
    Session,
}

impl Io {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Stream => "stream",
            Self::Duplex => "duplex",
            Self::Upload => "upload",
            Self::Download => "download",
            Self::Session => "session",
        }
    }

    /// Whether the generic JSON dispatcher can serve this operation.
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

/// Where an operation sits on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliProjection {
    /// Command path, e.g. `["issues", "list"]`.
    pub path: &'static [&'static str],
    /// Accepted alternatives for the final segment.
    pub aliases: &'static [&'static str],
}

impl CliProjection {
    pub fn invocation(&self) -> String {
        format!("loom {}", self.path.join(" "))
    }

    pub fn leaf(&self) -> &'static str {
        self.path.last().copied().unwrap_or_default()
    }

    pub fn group(&self) -> &'static [&'static str] {
        let len = self.path.len();
        &self.path[..len.saturating_sub(1)]
    }
}

/// A value the dispatcher resolves from the caller's session rather than
/// prompting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSource {
    RepoRoot,
    /// The branch row's opaque id.
    Branch,
    /// The branch's human name, e.g. `weaver/loom-fix-thing`.
    ///
    /// Used when provenance or user-facing references require the name.
    /// Distinct from [`ContextSource::Branch`] (the id): `issues.backlog` uses
    /// the name for provenance; `issues.create` uses the id.
    BranchName,
    Session,
}

impl ContextSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepoRoot => "repo_root",
            Self::Branch => "branch",
            Self::BranchName => "branch_name",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextField {
    pub name: &'static str,
    pub source: ContextSource,
}

/// The resolved session context a dispatcher fills context fields from.
///
/// Fetched once per invocation, avoiding redundant context lookups in each operation.
#[derive(Debug, Clone, Default)]
pub struct ContextValues {
    pub repo_root: String,
    pub branch: String,
    pub branch_name: String,
    pub session: String,
}

/// How a field's Rust type projects onto a command line.
///
/// Syntactic, decided by the macro from the type as written. Anything it does
/// not recognize is `Json` — one JSON literal for operands a flag can't express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandKind {
    Bool,
    Int,
    Str,
    OptBool,
    OptInt,
    OptStr,
    VecStr,
    VecInt,
    Json,
}

impl OperandKind {
    pub const fn is_multi(self) -> bool {
        matches!(self, Self::VecStr | Self::VecInt)
    }
}

/// How the command line spells one operand.
///
/// Presentation, and the only part of an [`Operand`] that is. A field with no
/// `CliSpelling` never reaches the command line — it is dispatcher-supplied
/// context, or explicitly held back with `#[operand(skip_cli)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliSpelling {
    pub positional: bool,
    pub long: &'static str,
    pub short: Option<char>,
    /// Read the value from the named file, or stdin for `-`.
    pub from_file: bool,
}

/// One caller-supplied field of an operation's input.
///
/// This is data, not code: the command line is built from this list by
/// `loom::cli::clap_bind`, so nothing in this crate knows what a parser is.
// No `PartialEq`: `default` is a function pointer, and comparing those is
// meaningless — two codegen units can give the same function two addresses.
#[derive(Debug, Clone, Copy)]
pub struct Operand {
    pub name: &'static str,
    pub kind: OperandKind,
    pub help: Option<&'static str>,
    /// A caller has to supply this one.
    pub required: bool,
    /// Filled by the dispatcher rather than the caller.
    pub context: Option<ContextSource>,
    /// The declared `#[operand(default = ...)]`, evaluated.
    ///
    /// The same expression is also this field's `serde` default, so a REST or
    /// MCP caller who omits the field gets the identical value without anyone
    /// consulting this. It is here because a command line has to *show* a
    /// default in `--help`, and because view flags are not `#[operation]`
    /// structs and so have no serde attributes of their own.
    pub default: Option<fn() -> Value>,
    pub cli: Option<CliSpelling>,
}

/// The caller-facing argument surface of an operation.
///
/// Derived, never written by hand: one struct yields the JSON body, the MCP
/// argument schema, and the operand list a command line is built from, so those
/// three cannot disagree.
pub trait Operands: Serialize + DeserializeOwned + Sized {
    /// Fields the dispatcher supplies from session context.
    const CONTEXT: &'static [ContextField];

    /// Every field, in declaration order.
    const OPERANDS: &'static [Operand];

    /// JSON Schema for what a caller may pass, with context fields elided.
    fn schema() -> Value;

    /// Fill dispatcher-supplied fields that the caller left unset.
    fn fill_context(&mut self, context: &ContextValues);
}

/// CLI-only flags. Never serialized, never sent.
///
/// Two kinds live here: how to print a result (`--mine`), and how the client
/// should behave while fetching one (`--interval`). What must NOT live here is
/// anything that changes *what the server returns* — that is an operand, or the
/// transports drift apart again.
pub trait ViewFlags: DeserializeOwned + Sized {
    const OPERANDS: &'static [Operand];
}

/// The view for an operation whose output needs no display options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoView;

impl ViewFlags for NoView {
    const OPERANDS: &'static [Operand] = &[];
}

// Declaring no flags still means decoding whatever the command line handed
// back, which is the empty object. A derived `Deserialize` on a unit struct
// insists on `null` and would fail every operation that takes no view flags —
// which is 211 of the 214.
impl<'de> Deserialize<'de> for NoView {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self)
    }
}

/// Everything a transport needs to know about one operation.
///
/// Note what is *absent*: no method, no path, no argument list, no command
/// string. The route is derived from `id`, and the arguments are read from the
/// input type through `schema`. There is no second copy to drift.
#[derive(Debug, Clone, Copy)]
pub struct OperationSpec {
    pub id: &'static str,
    pub bundle: &'static str,
    pub summary: &'static str,
    pub actor: ActorPolicy,
    pub scope: OperationScope,
    pub risk: OperationRisk,
    pub io: Io,
    pub grants: &'static [&'static str],
    pub cli: Option<CliProjection>,
    /// JSON Schema for what a caller may pass, context fields elided.
    pub schema: fn() -> Value,
    /// JSON Schema for what this operation returns.
    ///
    /// Derived from `Output`, the same way `schema` is derived from `Input`,
    /// so callers learn what comes back and not just what to send.
    pub output_schema: fn() -> Value,
    pub context: &'static [ContextField],
}

impl OperationSpec {
    /// The canonical REST route, derived from the identity.
    ///
    /// `issues.tags.set` is always `POST /api/issues/tags/set` — computed, not
    /// declared, so all surfaces report the same endpoint.
    pub fn path(&self) -> String {
        format!("/api/{}", self.id.replace('.', "/"))
    }

    pub fn method(&self) -> &'static str {
        match self.io {
            Io::Json | Io::Upload | Io::Session => "POST",
            Io::Stream | Io::Duplex | Io::Download => "GET",
        }
    }
}

/// A compile-time binding between one identity and its concrete types.
pub trait Operation: Send + Sync + 'static {
    type Input: Operands + Send + Sync + 'static;
    type Output: Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static;
    type View: ViewFlags + Send + Sync + 'static;

    const SPEC: &'static OperationSpec;
}

/// How an operation's input names the resource it acts on.
///
/// Scope is read off typed input rather than a URL, so REST, CLI, and MCP
/// reach the identical decision.
pub trait Scoped {
    fn scope_ref(&self) -> ScopeRef<'_>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRef<'a> {
    Session(&'a str),
    Branch(&'a str),
    Channel(&'a str),
    Repository(&'a str),
    Global,
}

/// Text presentation, shared by the CLI and MCP so one operation renders one way.
pub trait Render: Operation {
    /// How the CLI prints this operation's result.
    ///
    /// Defaults to the operation's own JSON, adequate for the long tail of
    /// administrative commands; bundles a human reads all day (`issues list`,
    /// `sessions list`) override it.
    fn text(output: &Self::Output, _view: &Self::View) -> String {
        serde_json::to_string_pretty(output)
            .unwrap_or_else(|error| format!("could not render result: {error}"))
    }
}

/// Remove dispatcher-supplied fields from a derived schema.
///
/// The fields stay on the wire — the server still receives `repo_root` — but
/// they are not something a caller may supply, so they must not appear in the
/// MCP tool schema or generated help.
pub fn strip_context_fields(schema: &mut Value, context: &[&str]) {
    if context.is_empty() {
        return;
    }
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for name in context {
            properties.remove(*name);
        }
    }
    if let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|value| value.as_str().is_none_or(|name| !context.contains(&name)));
        if required.is_empty() {
            object.remove("required");
        }
    }
}

/// Whether a context field still holds its default and should be filled.
pub trait Unset {
    fn is_unset(&self) -> bool;
}

impl Unset for String {
    fn is_unset(&self) -> bool {
        self.is_empty()
    }
}

impl<T> Unset for Option<T> {
    fn is_unset(&self) -> bool {
        self.is_none()
    }
}

pub fn is_unset<T: Unset>(value: &T) -> bool {
    value.is_unset()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_derived_from_identity_not_declared() {
        fn schema() -> Value {
            Value::Null
        }
        let spec = OperationSpec {
            id: "issues.tags.set",
            bundle: "issues",
            summary: "s",
            actor: ActorPolicy::SessionSelf,
            scope: OperationScope::Repository,
            risk: OperationRisk::Write,
            io: Io::Json,
            grants: &[],
            cli: None,
            schema,
            output_schema: schema,
            context: &[],
        };
        assert_eq!(spec.path(), "/api/issues/tags/set");
        assert_eq!(spec.method(), "POST");
    }

    #[test]
    fn context_fields_leave_the_caller_schema() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": { "repo_root": { "type": "string" }, "all": { "type": "boolean" } },
            "required": ["repo_root"]
        });
        strip_context_fields(&mut schema, &["repo_root"]);
        assert!(schema["properties"].get("repo_root").is_none());
        assert!(schema["properties"].get("all").is_some());
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn only_session_self_operations_may_reach_agents() {
        assert!(ActorPolicy::SessionSelf.agent_reachable());
        assert!(!ActorPolicy::User.agent_reachable());
        assert!(!ActorPolicy::Admin.agent_reachable());
        assert!(!ActorPolicy::Internal.agent_reachable());
    }
}
