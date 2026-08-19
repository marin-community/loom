//! A session's own pull-request association: which PR its branch is pinned
//! to, and re-fetching/labeling it through Loom's GitHub App credential.
//!
//! Distinct from `permissions.github.{grant,revoke}`, which govern *repository
//! access* for a session — a different resource entirely.
pub(super) use super::prelude;
pub mod clear;
pub mod labels;
pub mod refresh;
pub mod set;
