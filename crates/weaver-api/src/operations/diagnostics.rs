//! Operational health: the aggregated fleet snapshot (Settings →
//! Diagnostics) and build/process identity, for a human operator's debug
//! panel.
//!
//! Structured system state (session/profile counts, automation run health,
//! migration versions, build identity) — distinct from the `logs` bundle,
//! which is the log text itself. Both back the same Settings → Diagnostics
//! page.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;
pub mod get {
    use super::prelude::*;

    /// The aggregated fleet diagnostics snapshot: session/profile capacity,
    /// automation run health, migration state, and federation mappings.
    #[operation(
    id = "diagnostics.get",
    actor = User,
    scope = Global,
    risk = Read,
    grants = [],
)]
    pub struct Get;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
    pub struct Input {}

    pub type Output = DiagnosticsView;
}

pub mod status {
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
}

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <status::Status as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "diagnostics",
        label: "Diagnostics",
        operations: OPERATIONS,
    }
}
