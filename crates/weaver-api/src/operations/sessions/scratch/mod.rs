//! A session's Scratch directory: the files handed to it at launch and the
//! ones written to it while it runs.
pub(super) use super::prelude;
pub mod delete;
pub mod limits;
pub mod list;
pub mod write;
