use super::prelude::*;

/// Create an artifact or append a guarded revision.
///
/// The API accepts `content` as a JSON string. The CLI tool supports reading
/// from a file or stdin via `#[operand(from_file)]` for convenience, but this
/// is a client-side transformation — the wire format remains a JSON string.
#[operation(
    id = "artifacts.write",
    actor = SessionSelf,
    scope = Branch,
    risk = Write,
    grants = ["loom/artifacts/write@v1"],
    cli = "artifacts write",
    mcp = "loom_artifact::write",
)]
pub struct Write;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// The artifact body. On the command line this names a file, or `-`/omitted
    /// to read stdin.
    #[operand(positional, from_file)]
    pub content: String,
    /// Display title. Defaults to the existing title, or the name for a new
    /// artifact.
    pub title: Option<String>,
    /// Content kind, e.g. `markdown` or `image`.
    ///
    /// When omitted, the artifact keeps its current kind. This must be optional
    /// because a default value would silently change existing `plan` or `image`
    /// artifacts to markdown on every update that omits this field.
    pub kind: Option<String>,
    /// Optimistic-concurrency guard: `0` guards creation; a later revision
    /// number rejects a stale edit instead of silently overwriting it.
    pub base_rev: Option<i64>,
    /// Write the repository-shared artifact instead of this branch's own
    /// copy.
    #[operand(default = false)]
    pub repo: bool,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = ArtifactView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
