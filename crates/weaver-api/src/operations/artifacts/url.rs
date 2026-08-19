use super::prelude::*;

/// The externally-visible dashboard deep-link for an artifact.
///
/// The agent that just wrote the artifact holds only the loopback (or
/// wildcard) `$WEAVER_API` it was handed, and a `http://0.0.0.0:7878/…` link
/// printed after a write is useless to whoever reads it. Only the server
/// knows the externally-visible origin (the operator's `auth.base_url`, else
/// the request's own Host), so resolving it is the server's job — the twin of
/// `sessions.url`, whose `SessionUrlView` (`{url}`) this reuses unchanged.
#[operation(
    id = "artifacts.url",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
)]
pub struct Url;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    #[operand(positional)]
    pub name: String,
    /// Resolved from the calling session; not something a caller supplies.
    #[operand(context)]
    pub branch: String,
}

pub type Output = SessionUrlView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
