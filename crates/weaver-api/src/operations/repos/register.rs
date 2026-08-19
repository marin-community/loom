use super::prelude::*;

/// Register a repo in the managed store — add it to the clone allowlist. The
/// clone itself is lazy (it happens on first use); this just adds an entry.
#[operation(
    id = "repos.register",
    actor = User,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "repos register",
)]
pub struct Register;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// A GitHub `owner/name` slug or a clone URL.
    #[operand(positional)]
    pub repo: String,
}

pub type Output = RepoView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
