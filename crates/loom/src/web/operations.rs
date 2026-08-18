use std::future::Future;

use axum::{extract::Path, extract::State, http::StatusCode, Extension, Json};
use serde_json::Value;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::{ApiMetaView, ApiOperation, OperationSpec, OperationView};

use crate::{auth::Principal, AppState};

use super::{ApiResult, AppError};

/// Owned server context supplied to typed operation implementations.
///
/// Keeping this owned makes async handler futures `'static`, which allows the
/// binding checker to accept ordinary async functions without higher-ranked
/// borrowed-future machinery.
#[derive(Clone)]
pub(super) struct OperationContext {
    pub state: AppState,
    pub principal: Principal,
}

impl OperationContext {
    pub(super) fn new(state: AppState, principal: Principal) -> Self {
        Self { state, principal }
    }
}

/// Compile-checked association between a registered API operation and its Loom
/// implementation. Axum extraction remains an explicit adapter in phase one;
/// these bindings ensure the application function's input/output agree with
/// the cross-process contract.
#[derive(Debug, Clone, Copy)]
pub(super) struct ApiOperationBinding {
    pub operation: &'static OperationSpec,
}

pub(super) fn bind_operation<O, F, Fut>(handler: F) -> ApiOperationBinding
where
    O: ApiOperation,
    F: Fn(OperationContext, O::Input) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ApiResult<O::Output>> + Send + 'static,
{
    let _ = handler;
    ApiOperationBinding { operation: O::SPEC }
}

pub(super) fn validate_typed_bindings(
    bundle: &str,
    bindings: &[ApiOperationBinding],
) -> std::collections::BTreeSet<&'static str> {
    let registered = bindings
        .iter()
        .map(|binding| {
            assert_eq!(
                binding.operation.bundle, bundle,
                "typed operation {} belongs to the wrong bundle",
                binding.operation.id
            );
            binding.operation.id
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        registered.len(),
        bindings.len(),
        "typed API bindings for {bundle} contain duplicates"
    );
    registered
}

pub(super) fn validate_typed_bundle_bindings(bundle: &str, bindings: &[ApiOperationBinding]) {
    let expected = weaver_api::operations_for_bundle(bundle)
        .map(|operation| operation.id)
        .collect::<std::collections::BTreeSet<_>>();
    let registered = validate_typed_bindings(bundle, bindings);
    assert_eq!(
        registered, expected,
        "typed API bindings for {bundle} must cover every registered operation"
    );
}

pub(super) async fn api_meta() -> Json<ApiMetaView> {
    Json(ApiMetaView {
        product: "loom".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        operation_registry_version: 3,
        operations_url: "/api/operations".to_string(),
        openapi_url: "/api/openapi.json".to_string(),
    })
}

pub(super) async fn list_operations() -> Json<Vec<OperationView>> {
    Json(weaver_api::operation_views())
}

pub(super) async fn explain_operation(
    _: OperationContext,
    input: permission_operations::ExplainInput,
) -> ApiResult<OperationView> {
    weaver_api::operation(&input.operation)
        .map(OperationView::from)
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "operation not found"))
}

pub(super) async fn get_operation(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<OperationView>> {
    explain_operation(
        OperationContext::new(st, principal),
        permission_operations::ExplainInput { operation: id },
    )
    .await
    .map(Json)
}

pub(super) async fn openapi() -> Json<Value> {
    Json(weaver_api::operations::openapi_document(env!(
        "CARGO_PKG_VERSION"
    )))
}
