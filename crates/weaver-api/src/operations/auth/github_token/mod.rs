//! The caller's own GitHub personal-access token, injected into their
//! ordinary interactive sessions. Write-only: no operation here ever returns
//! the token value, only whether one is set.
pub(super) use super::prelude;
pub mod get;
pub mod remove;
pub mod set;
