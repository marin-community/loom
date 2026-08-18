//! weaver-api — the typed loom REST client and its request/response DTOs.
//!
//! This is the one cross-process API seam. The loom daemon owns the live
//! runtime (terminals, worktrees, the monitor); everything outside it — the `loom`
//! CLI, the Python binding, scripted watches — drives sessions through this
//! client over HTTP, never the runtime directly. The [`dto`] types are the single
//! definition of the wire contract the server serializes and these consumers
//! deserialize (and that `frontend/types.ts` mirrors).

pub mod capability;
pub mod client;
pub mod dto;
pub mod endpoint;
pub mod operations;

pub use capability::{require, CapabilityError};
pub use client::Client;
pub use dto::*;
pub use operations::{
    all_session_capabilities, mcp_tools, mcp_tools_ordered, operation, operation_bundles,
    operation_for_request, operation_input_schema, operation_views, operations,
    operations_for_bundle, session_capabilities_from_mcp, validate_operation_bundle_coverage,
    validate_operation_registry, ActorPolicy, ApiMetaView, ArgumentDefault, ArgumentKind,
    ArgumentSpec, ArgumentView, McpProjection, McpProjectionView, OperationBundle,
    OperationBundleFactory, OperationRisk, OperationScope, OperationSpec, OperationView,
    OPERATION_BUNDLE_FACTORIES,
};
