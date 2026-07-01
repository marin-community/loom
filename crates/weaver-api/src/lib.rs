//! weaver-api — the typed loom REST client and its request/response DTOs.
//!
//! This is the one cross-process API seam. The loom daemon owns the live
//! runtime (terminals, worktrees, the monitor); everything outside it — the `loom`
//! CLI, the Python binding, scripted overlookers — drives sessions through this
//! client over HTTP, never the runtime directly. The [`dto`] types are the single
//! definition of the wire contract the server serializes and these consumers
//! deserialize (and that `frontend/types.ts` mirrors).

pub mod capability;
pub mod client;
pub mod dto;
pub mod endpoint;

pub use capability::{require, CapabilityError};
pub use client::Client;
pub use dto::*;
