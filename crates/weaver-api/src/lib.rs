//! weaver-api — the typed loom REST client and its request/response DTOs.
//!
//! This is the one cross-process API seam. The loom daemon owns the live
//! runtime (terminals, worktrees, the monitor); everything outside it — the `loom`
//! CLI, the Python binding, scripted watches — drives sessions through this
//! client over HTTP, never the runtime directly. The [`dto`] types are the single
//! definition of the wire contract the server serializes and these consumers
//! deserialize (and that `frontend/types.ts` mirrors).

// The operation derives emit `::weaver_api::` paths so they work identically
// inside this crate and outside it.
extern crate self as weaver_api;

pub mod capability;
pub mod client;
pub mod dto;
pub mod endpoint;
pub mod operations;
pub mod render;

pub use capability::{require, CapabilityError};
pub use client::Client;
pub use dto::*;
pub use operations::{
    all_session_capabilities, operation, operation_bundles, operation_for_request,
    operation_input_schema, operation_views, operations, operations_for_bundle,
    session_capabilities_from_mcp, validate_operation_registry, ActorPolicy, ApiMetaView,
    CliProjection, ContextField, ContextSource, ContextValues, Io, NoView, Operands, Operation,
    OperationBundle, OperationBundleFactory, OperationRisk, OperationScope, OperationSpec,
    OperationView, Render, ScopeRef, Scoped, ViewFlags, OPERATION_BUNDLE_FACTORIES,
};
