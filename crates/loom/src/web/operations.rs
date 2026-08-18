use axum::{extract::Path, http::StatusCode, Json};
use serde_json::Value;
use weaver_api::{ApiMetaView, OperationView};

use super::{ApiResult, AppError};

pub(super) async fn api_meta() -> Json<ApiMetaView> {
    Json(ApiMetaView {
        product: "loom".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        operation_registry_version: 2,
        operations_url: "/api/operations".to_string(),
        openapi_url: "/api/openapi.json".to_string(),
    })
}

pub(super) async fn list_operations() -> Json<Vec<OperationView>> {
    Json(weaver_api::operation_views())
}

pub(super) async fn get_operation(Path(id): Path<String>) -> ApiResult<Json<OperationView>> {
    weaver_api::operation(&id)
        .map(OperationView::from)
        .map(Json)
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "operation not found"))
}

pub(super) async fn openapi() -> Json<Value> {
    Json(weaver_api::operations::openapi_document(env!(
        "CARGO_PKG_VERSION"
    )))
}
