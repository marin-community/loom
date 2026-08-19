use super::prelude::*;

/// An image artifact's decoded bytes, for an `<img src>`.
///
/// `io = Download` because the caller here is the browser's image loader: it
/// issues a `GET` and expects `image/png`, which no JSON envelope can be. The
/// same content is reachable as JSON through [`super::get`] — that is not
/// duplication but the two encodings of one artifact, and the declaration is
/// what says so.
#[operation(
    id = "artifacts.raw",
    actor = SessionSelf,
    scope = Branch,
    risk = Read,
    grants = ["loom/artifacts/read@v1"],
    io = Download,
)]
pub struct Raw;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The artifact's name.
    //
    // `serde(default)` because a download's operands arrive in the query string,
    // which axum extracts before any default-filling could run.
    #[serde(default)]
    #[operand(default = String::new())]
    pub name: String,
    /// Pin an immutable past revision instead of the latest.
    #[serde(default)]
    pub rev: Option<i64>,
    /// Resolved from the calling session; not something a caller supplies.
    #[serde(default)]
    #[operand(context)]
    pub branch: String,
}

pub type Output = ();

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Branch(&self.branch)
    }
}
