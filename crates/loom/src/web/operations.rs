//! The executable half of the operation registry.
//!
//! Binding a handler is what makes an operation exist on the server. There is no
//! later parity pass and no "declared but implemented elsewhere" state: a
//! descriptor without a [`register`] call fails startup validation, and a
//! `register` call without a descriptor does not compile.
//!
//! Authorization happens here, once, from typed input — actor, grants, and the
//! resource named by [`Scoped`]. The registry this replaces evaluated authority
//! twice on two different models, the second of which matched a URL string that
//! the operation's handler no longer served.

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use axum::{extract::State, http::StatusCode, Extension, Json, Router};
use serde_json::Value;
use weaver_api::operations::{
    ActorPolicy, ApiMetaView, Operation, OperationSpec, OperationView, ScopeRef, Scoped,
};

use crate::auth::Principal;
use crate::AppState;

use super::{ApiResult, AppError};

/// Owned server context handed to a bound operation.
///
/// Owned rather than borrowed so handler futures stay `'static`, which lets the
/// binding accept ordinary `async fn`s without higher-ranked lifetime machinery.
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
type Invoker = Arc<dyn Fn(OperationContext, Value) -> OperationFuture + Send + Sync>;

/// One executable entry: a descriptor and the implementation it names.
#[derive(Clone)]
pub(super) struct Bound {
    pub operation: &'static OperationSpec,
    invoke: Invoker,
}

impl Bound {
    pub(super) fn view(&self) -> OperationView {
        OperationView::from(self.operation)
    }
}

/// Bind a descriptor to its implementation.
///
/// The type parameters do the checking: `O::Input` and `O::Output` are the
/// operation's own types, so a handler cannot accept or return something the
/// declaration does not promise. JSON erasure happens only at this boundary.
pub(super) fn register<O, F, Fut>(handler: F) -> Bound
where
    O: Operation,
    O::Input: Scoped,
    F: Fn(OperationContext, O::Input) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = ApiResult<O::Output>> + Send + 'static,
{
    Bound {
        operation: O::SPEC,
        invoke: Arc::new(move |context, input| {
            let decoded = serde_json::from_value::<O::Input>(input);
            Box::pin(async move {
                let input = decoded.map_err(|error| {
                    AppError::bad_request(format!("invalid arguments for {}: {error}", O::SPEC.id))
                })?;
                authorize(&context, O::SPEC, input.scope_ref()).await?;
                let output = handler(context, input).await?;
                serde_json::to_value(output).map_err(|error| {
                    AppError::internal(format!("failed to encode result for {}", O::SPEC.id), error)
                })
            })
        }),
    }
}

/// The single authorization decision for a registered operation.
///
/// Every transport reaches this same function with the same typed input, so an
/// adapter cannot widen authority by choosing a different door.
async fn authorize(
    context: &OperationContext,
    operation: &'static OperationSpec,
    scope: ScopeRef<'_>,
) -> ApiResult<()> {
    if !actor_allows(&context.principal, operation) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "credential may not call this operation",
        ));
    }
    if !grants_allow(&context.principal, operation) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "credential lacks this operation's registered capability",
        ));
    }
    scope_allows(context, scope).await
}

fn actor_allows(principal: &Principal, operation: &OperationSpec) -> bool {
    use crate::auth::Grant;
    // An operation that needs no credential is reachable by everyone, including
    // a caller that has one. The converse is the point: `Grant::Anonymous`
    // reaches nothing else, so forgetting to authenticate a route cannot quietly
    // widen it — the declaration is what opens the door.
    if operation.actor == ActorPolicy::Anonymous {
        return true;
    }
    match &principal.grant {
        Grant::Anonymous => false,
        Grant::Admin => operation.actor != ActorPolicy::SessionOnly,
        // A human may stand in for a session on `SessionSelf`, but never on
        // `SessionOnly` — see that variant's doc comment.
        Grant::User => matches!(
            operation.actor,
            ActorPolicy::SessionSelf | ActorPolicy::User
        ),
        Grant::Automation { .. } => operation.actor == ActorPolicy::Internal,
        Grant::Session { .. } => matches!(
            operation.actor,
            ActorPolicy::SessionSelf | ActorPolicy::SessionOnly
        ),
    }
}

fn grants_allow(principal: &Principal, operation: &OperationSpec) -> bool {
    use crate::auth::Grant;
    let Grant::Session { capabilities, .. } = &principal.grant else {
        return true;
    };
    // `None` is the compatibility form minted before capability-bound
    // credentials; it is unrestricted by construction.
    capabilities.as_ref().is_none_or(|granted| {
        operation
            .grants
            .iter()
            .all(|required| granted.iter().any(|held| held == required))
    })
}

async fn scope_allows(context: &OperationContext, scope: ScopeRef<'_>) -> ApiResult<()> {
    let denied = || {
        AppError::new(
            StatusCode::FORBIDDEN,
            "credential cannot reach this resource",
        )
    };
    match scope {
        ScopeRef::Global => Ok(()),
        ScopeRef::Repository(repo_root) => {
            super::require_repo_access(&context.state, &context.principal, repo_root)
                .await
                .map_err(|_| denied())
        }
        ScopeRef::Branch(branch) => {
            super::require_branch_access(&context.state, &context.principal, branch)
                .await
                .map_err(|_| denied())
        }
        ScopeRef::Session(session) => {
            super::require_session_access(&context.state, &context.principal, session)
                .await
                .map_err(|_| denied())
        }
    }
}

/// The server's operation registry.
///
/// Every bundle contributes its bound operations here. Startup asserts that this
/// set exactly matches the descriptors in `weaver_api`, so a declared-but-unbound
/// operation is a boot failure rather than a 404 discovered in production.
pub(super) fn registry() -> Vec<Bound> {
    let mut bound = Vec::new();
    bound.extend(super::artifacts::bound_operations());
    bound.extend(super::auth::bound_operations());
    bound.extend(super::automation::bound_operations());
    bound.extend(super::channels::bound_operations());
    bound.extend(super::issues::bound_operations());
    bound.extend(super::logview::bound_operations());
    bound.extend(super::permission_requests::bound_operations());
    bound.extend(super::self_context::bound_operations());
    bound.extend(super::sessions::bound_operations());
    bound.extend(super::watches::bound_operations());
    bound.sort_by_key(|entry| entry.operation.id);
    bound
}

fn by_id() -> BTreeMap<&'static str, Bound> {
    registry()
        .into_iter()
        .map(|entry| (entry.operation.id, entry))
        .collect()
}

/// Assert that declarations and implementations are the same set.
///
/// This is the invariant that makes the registry trustworthy: it is impossible
/// to ship a descriptor that nothing serves, or to serve something undeclared.
pub(super) fn assert_registry_is_complete() {
    weaver_api::validate_operation_registry().expect("operation registry is structurally invalid");

    let bound = by_id();
    let declared = weaver_api::operations()
        .filter(|operation| operation.io.is_json())
        .map(|operation| operation.id)
        .collect::<Vec<_>>();

    let missing = declared
        .iter()
        .filter(|id| !bound.contains_key(**id))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "declared operations with no handler: {missing:?}"
    );

    let undeclared = bound
        .keys()
        .filter(|id| weaver_api::operation(id).is_none())
        .collect::<Vec<_>>();
    assert!(
        undeclared.is_empty(),
        "bound operations with no descriptor: {undeclared:?}"
    );
}

/// Mount every bound operation at its derived route.
///
/// Adding a registration creates its route; there is no second router table.
pub(super) fn mount(mut router: Router<AppState>) -> Router<AppState> {
    for entry in registry() {
        let path = entry
            .operation
            .path()
            .strip_prefix("/api")
            .unwrap_or_default()
            .to_string();
        router = router.route(
            &path,
            axum::routing::post({
                let entry = entry.clone();
                move |State(state): State<AppState>,
                      Extension(principal): Extension<Principal>,
                      Json(input): Json<Value>| {
                    let entry = entry.clone();
                    async move {
                        (entry.invoke)(OperationContext::new(state, principal), input)
                            .await
                            .map(Json)
                    }
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
        operation_registry_version: 5,
        operations_url: "/api/operations".to_string(),
        openapi_url: "/api/openapi.json".to_string(),
    })
}

pub(super) async fn list_operations() -> Json<Vec<OperationView>> {
    Json(weaver_api::operation_views())
}

/// One operation's descriptor, by id.
///
/// This used to be routed through the `permissions.explain` handler, which meant
/// a discovery endpoint carried a permission check for a resource it never read.
/// The descriptor is public information — it is what `/api/operations` already
/// returns in bulk — so it is served directly.
pub(super) async fn get_operation(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<Json<OperationView>> {
    weaver_api::operation(&id)
        .map(|operation| Json(OperationView::from(operation)))
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "no such operation"))
}

pub(super) async fn openapi() -> Json<serde_json::Value> {
    Json(weaver_api::operations::openapi_document(env!(
        "CARGO_PKG_VERSION"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_and_implementations_are_the_same_set() {
        assert_registry_is_complete();
    }

    #[test]
    fn routes_come_from_identity() {
        let list = weaver_api::operation("issues.list").unwrap();
        assert_eq!(list.path(), "/api/issues/list");
        // The defect this replaces: the descriptor declared `GET
        // /api/repos/issues`, a route the server had already stopped serving,
        // and authorization still keyed off that string.
        assert_eq!(list.method(), "POST");
    }
}
