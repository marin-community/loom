//! The GitHub App / OAuth sign-in setup. One App backs loom: its OAuth
//! client powers "Sign in with GitHub"; the same App's id and private key
//! power the `@loom` trigger.
pub(super) use super::prelude;
pub mod get;
pub mod set;
