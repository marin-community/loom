//! Provider-neutral inspection and administration of Loom's MCP registry.

use axum::http::StatusCode;
use weaver_api::operations::mcps as mcps_operations;
use weaver_api::{CustomMcpDeleteResult, CustomMcpReq, CustomMcpView, McpRegistryView};

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError};

/// `mcps.get`.
pub(super) async fn get_mcp_registry_operation(
    context: OperationContext,
    _input: mcps_operations::get::Input,
) -> ApiResult<McpRegistryView> {
    let st = &context.state;
    let mut registry = crate::mcp::registry();
    registry.custom_servers = crate::custom_mcp::list(&st.db).await?;
    Ok(registry)
}

/// `mcps.custom.list`.
pub(super) async fn list_custom_mcps_operation(
    context: OperationContext,
    _input: mcps_operations::custom::list::Input,
) -> ApiResult<Vec<CustomMcpView>> {
    Ok(crate::custom_mcp::list(&context.state.db).await?)
}

/// `mcps.custom.create`.
pub(super) async fn create_custom_mcp_operation(
    context: OperationContext,
    input: mcps_operations::custom::create::Input,
) -> ApiResult<CustomMcpView> {
    let req = CustomMcpReq {
        identity: input.identity,
        label: input.label,
        description: input.description,
        source: input.source,
        test_source: input.test_source,
        enabled: input.enabled,
    };
    let st = &context.state;
    let _resolver = st.launch_gate.acquire_resolver().await;
    if crate::custom_mcp::get(&st.db, req.identity.trim())
        .await?
        .is_some()
    {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            format!("custom MCP '{}' already exists", req.identity.trim()),
        ));
    }
    crate::custom_mcp::upsert(&st.db, &req)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))
}

/// `mcps.custom.get`. The `identity` field is the caller-supplied absolute
/// identity directly.
pub(super) async fn get_custom_mcp_operation(
    context: OperationContext,
    input: mcps_operations::custom::get::Input,
) -> ApiResult<CustomMcpView> {
    let st = &context.state;
    let identity = &input.identity;
    crate::custom_mcp::get(&st.db, identity)
        .await?
        .ok_or_else(|| AppError::not_found("custom MCP"))
}

/// `mcps.custom.update`. This upserts unconditionally: `custom_mcp::upsert`
/// creates a first revision for an identity that doesn't exist yet the same
/// way it revises an existing one, and this operation doesn't check
/// existence first.
pub(super) async fn update_custom_mcp_operation(
    context: OperationContext,
    input: mcps_operations::custom::update::Input,
) -> ApiResult<CustomMcpView> {
    let req = CustomMcpReq {
        identity: input.identity,
        label: input.label,
        description: input.description,
        source: input.source,
        test_source: input.test_source,
        enabled: input.enabled,
    };
    let st = &context.state;
    let _resolver = st.launch_gate.acquire_resolver().await;
    crate::custom_mcp::upsert(&st.db, &req)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))
}

/// `mcps.custom.delete`.
pub(super) async fn delete_custom_mcp_operation(
    context: OperationContext,
    input: mcps_operations::custom::delete::Input,
) -> ApiResult<CustomMcpDeleteResult> {
    let st = &context.state;
    let identity = input.identity;
    let _resolver = st.launch_gate.acquire_resolver().await;
    let removed = crate::custom_mcp::remove(&st.db, &identity)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    if removed {
        Ok(CustomMcpDeleteResult {
            deleted: true,
            identity,
        })
    } else {
        Err(AppError::not_found("custom MCP"))
    }
}

// ---------------------------------------------------------------------------
// Operation registry — `mcps.*` and `mcps.custom.*`, bound onto
// `weaver_api::operations::mcps`.
// ---------------------------------------------------------------------------

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<mcps_operations::get::Get, _, _>(get_mcp_registry_operation),
        register::<mcps_operations::custom::list::List, _, _>(list_custom_mcps_operation),
        register::<mcps_operations::custom::get::Get, _, _>(get_custom_mcp_operation),
        register::<mcps_operations::custom::create::Create, _, _>(create_custom_mcp_operation),
        register::<mcps_operations::custom::update::Update, _, _>(update_custom_mcp_operation),
        register::<mcps_operations::custom::delete::Delete, _, _>(delete_custom_mcp_operation),
    ]
}
