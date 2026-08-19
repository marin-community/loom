use super::prelude::*;

/// Clone one profile's reviewed policy into a new insert-only profile,
/// optionally composing its write-only environment in the same transaction.
/// Loom guards both the source profile's revision and the resolver
/// fingerprint the caller reviewed; a drift in either returns a fresh
/// preview instead of silently applying a stale composition.
#[operation(
    id = "profiles.clone",
    actor = Admin,
    scope = Global,
    risk = Write,
    grants = [],
    cli = "profiles clone",
)]
pub struct Clone;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// The profile being cloned.
    #[operand(positional)]
    pub source: String,
    /// The new profile's name.
    #[operand(positional)]
    pub name: String,
    /// Revision of `source` the caller reviewed; a 409 with a fresh preview
    /// means it has since changed.
    pub expected_profile_revision: i64,
    /// Resolver fingerprint from the composition the caller reviewed.
    pub expected_resolver_revision: String,
    /// Fields to layer over the source profile for this one resolution.
    #[operand(json, default = LaunchOverrides::default())]
    pub overrides: LaunchOverrides,
    /// Optional fully edited profile proposal. Omitted copies the source
    /// profile's policy verbatim; source revision and environment copy
    /// remain server-owned and atomic either way.
    #[operand(json, default = None)]
    pub template: Option<ProfileReq>,
    /// Copy the source's write-only environment; ignored when `environment` is present.
    #[operand(default = false)]
    pub copy_environment: bool,
    /// Explicit write-only environment composition for the clone.
    #[operand(json, default = None)]
    pub environment: Option<CloneProfileEnvironmentReq>,
}

pub type Output = ProfileView;

impl Scoped for Input {
    fn scope_ref(&self) -> ScopeRef<'_> {
        ScopeRef::Global
    }
}
