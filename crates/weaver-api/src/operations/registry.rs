//! The transport-neutral operation contract.
//!
//! One operation is one declaration. This module holds the vocabulary that
//! declaration is written in and the traits the derives target; the projections
//! onto REST, CLI, and MCP read every fact they need from here.
//!
//! The invariant that makes the registry worth having: an entry cannot describe
//! an operation it does not also define. `OperationSpec` carries no free-form
//! route, argument list, or command string — the schema and the clap surface are
//! function pointers into the operation's own `Input` type, so there is nothing
//! to keep in agreement by hand.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

/// Who may call an operation.
///
/// This is the axis that replaces excluding administrative or human-only
/// endpoints from the registry: an operator-only action is `Admin`, not absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorPolicy {
    SessionSelf,
    /// The session credential itself, and nothing else — not even a human.
    ///
    /// `SessionSelf` lets a human operator stand in for a session, which is
    /// right for almost everything: an operator reading a session's issues is
    /// ordinary. It is wrong for operations that hand back *credential
    /// material*, where standing in means one user obtaining another user's
    /// session token. `permissions.github.token` is that case, and the route it
    /// replaces refused Admin and User outright.
    SessionOnly,
    User,
    Admin,
    Internal,
    /// Reachable with no credential at all.
    ///
    /// This is how you log in: `auth.login` must run before a principal exists.
    /// Declaring it makes the unauthenticated surface enumerable — previously it
    /// was a path prefix (`/auth/...`) matched in middleware, so the answer to
    /// "what can an anonymous caller reach?" lived in a string comparison rather
    /// than anywhere you could read or test. `anonymous_operations_are_pinned`
    /// asserts the exact set, so widening it requires editing a test that says
    /// so in as many words.
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
    /// Used by the invariant that forbids an MCP projection on a human-only
    /// operation, which is how "agents cannot approve their own permission
    /// requests" stops being an absence and becomes a checked property.
    pub const fn agent_reachable(self) -> bool {
        matches!(self, Self::SessionSelf | Self::SessionOnly)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationScope {
    Session,
    Branch,
    Repository,
    /// Not scoped to one resource — fleet-wide reads and administration.
    Global,
}

impl OperationScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Branch => "branch",
            Self::Repository => "repository",
            Self::Global => "global",
        }
    }
}

/// How an operation's response is encoded.
///
/// This is the *only* axis on which a registered operation may be special. It
/// is a field rather than an escape hatch so that streaming and upload
/// endpoints keep their descriptor, their typed input, and their authorization,
/// and cannot become the dumping ground the old `HttpBinding::Custom` was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Io {
    /// JSON request, JSON response. The overwhelming majority.
    Json,
    /// Server-sent events. The CLI projects `--follow`; MCP normally abstains.
    Stream,
    /// Bidirectional websocket (terminals, the IDE proxy).
    Duplex,
    /// Raw request body: multipart or octet-stream, streamed rather than
    /// wrapped in JSON.
    ///
    /// This describes the *wire*, not how a CLI gathers an argument. An
    /// operation whose content is a JSON string stays `Json` even when the CLI
    /// lets you name a file for it — that is `#[operand(from_file)]`, a
    /// client-side affordance. Getting this backwards silently deletes an MCP
    /// tool, because a non-JSON operation may not have one.
    Upload,
    /// Raw response body: bytes plus a guessed content type.
    ///
    /// The mirror image of `Upload`, and it exists for the same reason: a
    /// browser reaches these through an `<img src>` or a download link, which is
    /// a `GET` for bytes and cannot be a JSON POST. Operands arrive in the query
    /// string, exactly as a stream's do.
    ///
    /// Like every other non-JSON encoding this is about the *wire*. An operation
    /// that answers with base64 inside a JSON envelope stays `Json`.
    Download,
    /// JSON body plus a browser session-cookie effect.
    ///
    /// Logging in and out are ordinary operations in every respect a caller can
    /// see — they have schemas, they appear in the surface, they are authorized
    /// the same way — but their *response* has to carry a `Set-Cookie` an
    /// HttpOnly session depends on, and a JSON body cannot express that. So they
    /// are served by a transport-specific route mounted at the same derived path
    /// rather than by the generic dispatcher.
    ///
    /// The point of naming it is that "this operation is not on the generic
    /// path" becomes a declared fact with a test behind it, instead of a route
    /// someone forgot to migrate.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpProjection {
    pub server: &'static str,
    pub tool: &'static str,
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
    /// Distinct from [`ContextSource::Branch`] because they are not
    /// interchangeable and confusing them is silent: a field annotated `branch`
    /// that is compared against a name simply never matches. `issues.backlog`
    /// stores the name for provenance while `issues.create` keys off the id.
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
/// Fetched once per invocation. This is what removes the extra `self_context()`
/// round-trip every MCP tool used to make inside its own `project_input`.
#[derive(Debug, Clone, Default)]
pub struct ContextValues {
    pub repo_root: String,
    pub branch: String,
    pub branch_name: String,
    pub session: String,
}

/// The caller-facing argument surface of an operation.
///
/// Derived, never written by hand: one struct yields the JSON body, the MCP
/// argument schema, and the clap flags, so those three cannot disagree.
pub trait Operands: Serialize + DeserializeOwned + Sized {
    /// Fields the dispatcher supplies from session context.
    const CONTEXT: &'static [ContextField];

    /// JSON Schema for what a caller may pass, with context fields elided.
    fn schema() -> Value;

    /// Add this operand set to a clap command.
    fn augment(cmd: clap::Command) -> clap::Command;

    /// Rebuild from parsed matches. Context fields are left at their default
    /// and populated by [`Operands::fill_context`].
    fn from_matches(matches: &clap::ArgMatches) -> Result<Self, String>;

    /// Fill dispatcher-supplied fields that the caller left unset.
    fn fill_context(&mut self, context: &ContextValues);

    /// The declared defaults, as JSON, for fields a caller may omit.
    ///
    /// `#[operand(default = ...)]` used to reach only clap, so a REST caller
    /// that omitted a defaulted field got `missing field `prune`` — a default
    /// that did not exist on the wire it was declared for. This is built from
    /// the same expression the command line uses, so the two cannot diverge.
    fn wire_defaults() -> Value {
        Value::Object(Default::default())
    }
}

/// CLI-only flags. Never serialized, never sent.
///
/// Two kinds live here: how to print a result (`--mine`), and how the client
/// should behave while fetching one (`--interval`). What must NOT live here is
/// anything that changes *what the server returns* — that is an operand, or the
/// transports drift apart again.
pub trait ViewFlags: Sized {
    fn augment(cmd: clap::Command) -> clap::Command;
    fn from_matches(matches: &clap::ArgMatches) -> Result<Self, String>;
}

/// The view for an operation whose output needs no display options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoView;

impl ViewFlags for NoView {
    fn augment(cmd: clap::Command) -> clap::Command {
        cmd
    }

    fn from_matches(_: &clap::ArgMatches) -> Result<Self, String> {
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
    pub mcp: Option<McpProjection>,
    pub schema: fn() -> Value,
    pub context: &'static [ContextField],
}

impl OperationSpec {
    /// The canonical REST route, derived from the identity.
    ///
    /// `issues.tags.set` is always `POST /api/issues/tags/set`. Because it is
    /// computed rather than declared, `loom help` and `GET /api/operations` can
    /// no longer report different endpoints for the same operation.
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
    type Output: Serialize + DeserializeOwned + Send + Sync + 'static;
    type View: ViewFlags + Send + Sync + 'static;

    const SPEC: &'static OperationSpec;
}

/// How an operation's input names the resource it acts on.
///
/// This replaces synthesizing a URL and re-running a path matcher: scope is read
/// off typed input, so REST, CLI, and MCP reach the identical decision.
pub trait Scoped {
    fn scope_ref(&self) -> ScopeRef<'_>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRef<'a> {
    Session(&'a str),
    Branch(&'a str),
    Repository(&'a str),
    Global,
}

/// Text presentation, shared by the CLI and MCP so one operation renders one way.
pub trait Render: Operation {
    /// How the CLI prints this operation's result.
    ///
    /// The default is the operation's own JSON, which is honest and complete for
    /// the long tail of administrative commands. Bundles a human reads all day —
    /// `issues list`, `sessions list` — override it. The point of the default is
    /// that adding an operation never requires writing a renderer before the
    /// command works, which is how the old surface ended up advertising commands
    /// that did not exist.
    fn text(output: &Self::Output, _view: &Self::View) -> String {
        serde_json::to_string_pretty(output)
            .unwrap_or_else(|error| format!("could not render result: {error}"))
    }
}

/// Remove dispatcher-supplied fields from a derived schema.
///
/// The fields stay on the wire — the server still receives `repo_root` — they
/// simply are not something a caller may supply, so they must not appear in the
/// MCP tool schema or in generated help.
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
        // The defect this replaces: `loom help` printed a declared path while
        // `--json` printed a computed one, and they disagreed for every
        // generated operation.
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
            mcp: None,
            schema,
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
