use super::prelude::*;

/// Build and process identity for a human operator's debug panel: which
/// version and image are running, and since when.
#[operation(
    id = "diagnostics.status",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
pub struct Status;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {}

/// A small "what am I looking at" status blob for the debug panel: build and
/// image identity plus process identity, so both deploys and restarts are
/// attributable.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Output {
    pub version: String,
    pub build_revision: String,
    pub build_profile: String,
    /// Digest-pinned image reference when a container deployment supplies
    /// one.
    pub image: Option<String>,
    pub pid: u32,
    /// When this process started capturing logs (≈ process start), RFC3339.
    pub started_at: String,
}

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
