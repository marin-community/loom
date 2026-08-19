//! Answering a live, in-flight ACP permission prompt.
//!
//! Distinct from `permissions.requests.{approve,deny}`, which resolve
//! out-of-band request records.
pub(super) use super::prelude;
pub mod answer;
