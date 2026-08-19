//! Provider-neutral inspection and administration of Loom's MCP registry.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use weaver_api::operations::mcps as mcps_operations;
use weaver_api::{CustomMcpDeleteResult, CustomMcpReq, CustomMcpView, McpRegistryView};

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

async fn mcp_registry_core(st: &AppState) -> ApiResult<McpRegistryView> {
    let mut registry = crate::mcp::registry();
    registry.custom_servers = crate::custom_mcp::list(&st.db).await?;
    Ok(registry)
}

pub(super) async fn list_mcps(State(st): State<AppState>) -> ApiResult<Json<McpRegistryView>> {
    Ok(Json(mcp_registry_core(&st).await?))
}

/// `mcps.get` — the twin of [`list_mcps`].
pub(super) async fn get_mcp_registry_operation(
    context: OperationContext,
    _input: mcps_operations::get::Input,
) -> ApiResult<McpRegistryView> {
    mcp_registry_core(&context.state).await
}

pub(super) async fn list_custom_mcps(
    State(st): State<AppState>,
) -> ApiResult<Json<Vec<CustomMcpView>>> {
    Ok(Json(crate::custom_mcp::list(&st.db).await?))
}

/// `mcps.custom.list` — the twin of [`list_custom_mcps`].
pub(super) async fn list_custom_mcps_operation(
    context: OperationContext,
    _input: mcps_operations::custom::list::Input,
) -> ApiResult<Vec<CustomMcpView>> {
    Ok(crate::custom_mcp::list(&context.state.db).await?)
}

async fn create_custom_mcp_core(st: &AppState, req: CustomMcpReq) -> ApiResult<CustomMcpView> {
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

pub(super) async fn create_custom_mcp(
    State(st): State<AppState>,
    Json(req): Json<CustomMcpReq>,
) -> ApiResult<(StatusCode, Json<CustomMcpView>)> {
    let created = create_custom_mcp_core(&st, req).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `mcps.custom.create` — the twin of [`create_custom_mcp`].
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
    create_custom_mcp_core(&context.state, req).await
}

fn identity_from_path(path: &str) -> String {
    format!("/{}", path.trim_matches('/'))
}

async fn get_custom_mcp_core(st: &AppState, identity: &str) -> ApiResult<CustomMcpView> {
    crate::custom_mcp::get(&st.db, identity)
        .await?
        .ok_or_else(|| AppError::not_found("custom MCP"))
}

pub(super) async fn get_custom_mcp(
    State(st): State<AppState>,
    Path(identity): Path<String>,
) -> ApiResult<Json<CustomMcpView>> {
    Ok(Json(
        get_custom_mcp_core(&st, &identity_from_path(&identity)).await?,
    ))
}

/// `mcps.custom.get` — the twin of [`get_custom_mcp`]. The operation's
/// `identity` is the caller-supplied absolute identity directly; the legacy
/// route instead reconstructed it from a URL wildcard path segment.
pub(super) async fn get_custom_mcp_operation(
    context: OperationContext,
    input: mcps_operations::custom::get::Input,
) -> ApiResult<CustomMcpView> {
    get_custom_mcp_core(&context.state, &input.identity).await
}

async fn update_custom_mcp_core(st: &AppState, req: CustomMcpReq) -> ApiResult<CustomMcpView> {
    let _resolver = st.launch_gate.acquire_resolver().await;
    crate::custom_mcp::upsert(&st.db, &req)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))
}

pub(super) async fn put_custom_mcp(
    State(st): State<AppState>,
    Path(identity): Path<String>,
    Json(mut req): Json<CustomMcpReq>,
) -> ApiResult<Json<CustomMcpView>> {
    req.identity = identity_from_path(&identity);
    Ok(Json(update_custom_mcp_core(&st, req).await?))
}

/// `mcps.custom.update` — the twin of [`put_custom_mcp`]. Like the legacy
/// route, this upserts unconditionally: `custom_mcp::upsert` creates a first
/// revision for an identity that doesn't exist yet the same way it revises an
/// existing one, and neither the route nor this operation checks existence
/// first.
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
    update_custom_mcp_core(&context.state, req).await
}

async fn delete_custom_mcp_core(
    st: &AppState,
    identity: String,
) -> ApiResult<CustomMcpDeleteResult> {
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

pub(super) async fn delete_custom_mcp(
    State(st): State<AppState>,
    Path(identity): Path<String>,
) -> ApiResult<StatusCode> {
    delete_custom_mcp_core(&st, identity_from_path(&identity)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `mcps.custom.delete` — the twin of [`delete_custom_mcp`].
pub(super) async fn delete_custom_mcp_operation(
    context: OperationContext,
    input: mcps_operations::custom::delete::Input,
) -> ApiResult<CustomMcpDeleteResult> {
    delete_custom_mcp_core(&context.state, input.identity).await
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
