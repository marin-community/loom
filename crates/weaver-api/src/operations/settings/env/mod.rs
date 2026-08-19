//! The protected `default` profile's environment — a flat name/value
//! compatibility facade predating per-profile environment stores. See
//! `loom_store::agent_env` for the storage this projects.

pub(super) use super::prelude;

pub mod delete;
pub mod list;
pub mod set;
