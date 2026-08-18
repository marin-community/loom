use std::{future::Future, pin::Pin, sync::Arc};

use axum::{extract::Path, extract::State, http::StatusCode, Extension, Json, Router};
use serde_json::Value;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::{ApiMetaView, ApiOperation, HttpBinding, OperationSpec, OperationView};

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

type OperationFuture = Pin<Box<dyn Future<Output = ApiResult<Value>> + Send>>;
type OperationInvoker = Arc<dyn Fn(OperationContext, Value) -> OperationFuture + Send + Sync>;

/// One entry in Loom's executable operation registry.
///
/// A routine operation owns its descriptor and its type-erased implementation in
/// the same value.  The erasure happens only at the JSON transport boundary:
/// `register_operation` still compile-checks the concrete input, output, and async
/// handler signature.  Custom entries describe the deliberately bespoke Axum
/// routes used for streams, files, atomic bulk endpoints, and other shapes that
/// do not fit the ordinary JSON operation contract.
#[derive(Clone)]
pub(super) struct RegisteredOperation {
    pub operation: &'static OperationSpec,
    invoke: Option<OperationInvoker>,
}

impl RegisteredOperation {
    fn custom(operation: &'static OperationSpec) -> Self {
        assert_eq!(
            operation.http_binding,
            HttpBinding::Custom,
            "generated operation {} must bind an implementation",
            operation.id
        );
        Self {
            operation,
            invoke: None,
        }
    }

    pub(super) fn is_bound(&self) -> bool {
        self.invoke.is_some()
    }

    fn api_path(&self) -> String {
        format!("/{}", self.operation.id.replace('.', "/"))
    }

    fn view(&self) -> OperationView {
        OperationView::from(self.operation)
    }

    async fn invoke(&self, context: OperationContext, input: Value) -> ApiResult<Value> {
        let invoke = self.invoke.as_ref().ok_or_else(|| {
            AppError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "operation uses a custom HTTP adapter",
            )
        })?;
        invoke(context, input).await
    }
}

/// Bind a typed operation descriptor and its implementation as one registry
/// entry.  There is no later parity pass: a routine operation does not exist on the
/// server unless this function has produced its executable registration.
pub(super) fn register_operation<O, F, Fut>(handler: F) -> RegisteredOperation
where
    O: ApiOperation,
    F: Fn(OperationContext, O::Input) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = ApiResult<O::Output>> + Send + 'static,
{
    assert_eq!(
        O::SPEC.http_binding,
        HttpBinding::Generated,
        "custom operation {} must use an explicit HTTP adapter",
        O::SPEC.id
    );
    RegisteredOperation {
        operation: O::SPEC,
        invoke: Some(Arc::new(move |context, input| {
            let decoded = serde_json::from_value::<O::Input>(input);
            Box::pin(async move {
                let input = decoded.map_err(|error| {
                    AppError::bad_request(format!("invalid arguments for {}: {error}", O::SPEC.id))
                })?;
                // Reuse the semantic route's scope authorization before
                // entering application code. The canonical operation endpoint
                // therefore has the branch/session/repository boundary declared
                // by the typed contract.
                let request = O::authorization_request(&input).map_err(|error| {
                    AppError::bad_request(format!("invalid arguments for {}: {error}", O::SPEC.id))
                })?;
                let method =
                    axum::http::Method::from_bytes(request.method.as_bytes()).map_err(|error| {
                        AppError::internal("registered operation has an invalid method", error)
                    })?;
                if !super::auth::grant_allows(
                    &context.state,
                    &context.principal,
                    &method,
                    &request.path,
                )
                .await
                {
                    return Err(AppError::new(
                        StatusCode::FORBIDDEN,
                        "credential lacks this operation's registered capability or scope",
                    ));
                }
                let output = handler(context, input).await?;
                serde_json::to_value(output).map_err(|error| {
                    AppError::internal(format!("failed to encode result for {}", O::SPEC.id), error)
                })
            })
        })),
    }
}

fn register_custom_operation(id: &'static str) -> RegisteredOperation {
    RegisteredOperation::custom(
        weaver_api::operation(id)
            .unwrap_or_else(|| panic!("custom operation {id} has no descriptor")),
    )
}

fn register_custom_bundle(bundle: &'static str) -> impl Iterator<Item = RegisteredOperation> {
    weaver_api::operations_for_bundle(bundle).map(RegisteredOperation::custom)
}

/// The single server-side operation registry. Bound operations and intentional
/// custom adapters share one catalogue; discovery, authorization, OpenAPI, and
/// generic invocation all enumerate this value.
pub(super) fn registry() -> Vec<RegisteredOperation> {
    // These resource groups still use specialized HTTP adapters. Naming them
    // here is the explicit escape hatch; adding a descriptor elsewhere does
    // not silently turn it into a server operation.
    let mut registered = register_custom_bundle("sessions")
        .chain(register_custom_bundle("channels"))
        .chain(register_custom_bundle("artifacts"))
        .collect::<Vec<_>>();

    registered.extend(super::issues::registered_operations());
    registered.push(register_custom_operation("issues.actions"));

    registered.extend(super::permission_requests::registered_operations());
    registered.push(register_operation::<permission_operations::Explain, _, _>(
        explain_operation,
    ));
    registered.extend(
        [
            "permissions.requests.approve",
            "permissions.requests.deny",
            "permissions.github.grant",
            "permissions.github.revoke",
            "permissions.github.token",
            "permissions.github.restricted.invoke",
        ]
        .into_iter()
        .map(register_custom_operation),
    );

    registered.sort_by_key(|registration| registration.operation.id);
    assert!(
        registered
            .windows(2)
            .all(|pair| pair[0].operation.id != pair[1].operation.id),
        "server operation registry contains duplicate ids"
    );
    registered
}

pub(super) fn registered_operation(id: &str) -> Option<RegisteredOperation> {
    registry()
        .into_iter()
        .find(|registration| registration.operation.id == id)
}

pub(super) fn bound_operation_for_request(
    method: &axum::http::Method,
    path: &str,
) -> Option<RegisteredOperation> {
    (*method == axum::http::Method::POST).then_some(())?;
    registry().into_iter().find(|registration| {
        if !registration.is_bound() {
            return false;
        }
        let operation_path = registration.api_path();
        path == operation_path || path == format!("/api{operation_path}")
    })
}

/// Mount every routine operation from the executable registry.  Adding a
/// bound registration creates its API route; there is no second router table
/// to update.
pub(super) fn mount_registered_operations(mut router: Router<AppState>) -> Router<AppState> {
    for registration in registry().into_iter().filter(RegisteredOperation::is_bound) {
        let path = registration.api_path();
        router = router.route(
            &path,
            axum::routing::post({
                let registration = registration.clone();
                move |State(st): State<AppState>,
                      Extension(principal): Extension<Principal>,
                      Json(input): Json<Value>| {
                    let registration = registration.clone();
                    async move { invoke_registration(registration, st, principal, input).await }
                }
            }),
        );
    }
    router
}

pub(super) async fn api_meta() -> Json<ApiMetaView> {
    Json(ApiMetaView {
        product: "loom".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        operation_registry_version: 4,
        operations_url: "/api/operations".to_string(),
        openapi_url: "/api/openapi.json".to_string(),
    })
}

pub(super) async fn list_operations() -> Json<Vec<OperationView>> {
    Json(registry().iter().map(RegisteredOperation::view).collect())
}

pub(super) async fn explain_operation(
    _: OperationContext,
    input: permission_operations::ExplainInput,
) -> ApiResult<OperationView> {
    registered_operation(&input.operation)
        .map(|registration| registration.view())
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

async fn invoke_registration(
    registration: RegisteredOperation,
    st: AppState,
    principal: Principal,
    input: Value,
) -> ApiResult<Json<Value>> {
    if !super::auth::operation_grant_allows(&principal, registration.operation) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "credential lacks this operation's registered capability or scope",
        ));
    }
    registration
        .invoke(OperationContext::new(st, principal), input)
        .await
        .map(Json)
}

pub(super) async fn openapi() -> Json<Value> {
    let operations = registry()
        .iter()
        .map(RegisteredOperation::view)
        .collect::<Vec<_>>();
    Json(weaver_api::operations::openapi_document_for_views(
        env!("CARGO_PKG_VERSION"),
        &operations,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_distinguishes_bound_operations_from_explicit_custom_adapters() {
        let list = registered_operation("issues.list").unwrap();
        assert!(list.is_bound());
        assert_eq!(list.view().method, "POST");
        assert_eq!(list.view().path, "/api/issues/list");

        let bulk = registered_operation("issues.actions").unwrap();
        assert!(!bulk.is_bound());
        assert_eq!(bulk.view().path, "/api/issues/actions");
    }
}
