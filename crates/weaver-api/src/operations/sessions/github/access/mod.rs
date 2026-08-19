//! Which repositories a session may reach through Loom's GitHub credential.
//!
//! The read half only. Granting and revoking are `permissions.github.grant` /
//! `.revoke`, which live with the other permission decisions a human makes
//! about an agent.
pub(super) use super::prelude;
pub mod list;
