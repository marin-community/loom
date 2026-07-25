//! Provider-neutral inspection and administration of Loom's MCP registry.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use weaver_api::{CustomMcpReq, CustomMcpView, McpRegistryView};

use super::{ApiResult, AppError, AppState};

pub(super) async fn list_mcps(State(st): State<AppState>) -> ApiResult<Json<McpRegistryView>> {
    let mut registry = crate::mcp::registry();
    registry.custom_servers = crate::custom_mcp::list(&st.db).await?;
    Ok(Json(registry))
}

pub(super) async fn list_custom_mcps(
    State(st): State<AppState>,
) -> ApiResult<Json<Vec<CustomMcpView>>> {
    Ok(Json(crate::custom_mcp::list(&st.db).await?))
}

pub(super) async fn create_custom_mcp(
    State(st): State<AppState>,
    Json(req): Json<CustomMcpReq>,
) -> ApiResult<(StatusCode, Json<CustomMcpView>)> {
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
    let value = crate::custom_mcp::upsert(&st.db, &req)
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    Ok((StatusCode::CREATED, Json(value)))
}

fn identity_from_path(path: &str) -> String {
    format!("/{}", path.trim_matches('/'))
}

pub(super) async fn get_custom_mcp(
    State(st): State<AppState>,
    Path(identity): Path<String>,
) -> ApiResult<Json<CustomMcpView>> {
    crate::custom_mcp::get(&st.db, &identity_from_path(&identity))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found("custom MCP"))
}

pub(super) async fn put_custom_mcp(
    State(st): State<AppState>,
    Path(identity): Path<String>,
    Json(mut req): Json<CustomMcpReq>,
) -> ApiResult<Json<CustomMcpView>> {
    let _resolver = st.launch_gate.acquire_resolver().await;
    req.identity = identity_from_path(&identity);
    Ok(Json(
        crate::custom_mcp::upsert(&st.db, &req)
            .await
            .map_err(|error| AppError::bad_request(error.to_string()))?,
    ))
}

pub(super) async fn delete_custom_mcp(
    State(st): State<AppState>,
    Path(identity): Path<String>,
) -> ApiResult<StatusCode> {
    let _resolver = st.launch_gate.acquire_resolver().await;
    let removed = crate::custom_mcp::remove(&st.db, &identity_from_path(&identity))
        .await
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("custom MCP"))
    }
}
