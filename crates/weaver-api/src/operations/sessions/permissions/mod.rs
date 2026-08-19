//! Answering a live, in-flight ACP permission prompt — distinct from
//! `permissions.requests.{approve,deny}`, which resolve an out-of-band
//! request record rather than a `session/request_permission` call an adapter
//! is presently blocked on.
pub(super) use super::prelude;
pub mod answer;
