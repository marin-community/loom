use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{self, KeepAlive, Sse},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use weaver_api::{
    CreateSessionGroupReq, CreateSessionSpaceReq, DeleteSessionGroupReq, DeleteSessionSpaceReq,
    MoveSessionsReq, ReorderSessionLayoutReq, RestoreSessionGroupsReq, SessionGroupPreferenceReq,
    SessionLayoutView, SessionPlacementSelectorKind, SetSessionPlacementDefaultReq,
    UpdateSessionGroupReq, UpdateSessionSpaceReq,
};

use weaver_api::operations::session_layout::{
    defaults, events, get, groups, r#move, reorder, restore, spaces,
};

use crate::auth::Principal;
use crate::session_layout::{self, MutationError};

use super::operations::{register, Bound, OperationContext};
use super::{ApiResult, AppError, AppState};

fn require_human(principal: &Principal) -> ApiResult<()> {
    if principal.is_human() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "shared session layout requires a human credential",
        ))
    }
}

async fn mutation_response(
    st: &AppState,
    username: &str,
    result: Result<SessionLayoutView, MutationError>,
) -> ApiResult<Json<SessionLayoutView>> {
    match result {
        Ok(layout) => {
            session_layout::publish_invalidation(&st.db, &st.bus, layout.revision).await;
            Ok(Json(layout))
        }
        Err(MutationError::Conflict) => {
            let layout = session_layout::get_layout(&st.db, username).await?;
            Err(AppError::conflict("session layout revision changed")
                .with_fields(json!({ "layout": layout })))
        }
        Err(MutationError::Invalid(message)) => Err(AppError::bad_request(message)),
        Err(MutationError::Internal(error)) => Err(error.into()),
    }
}

pub(super) async fn get_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    Ok(Json(
        session_layout::get_layout(&st.db, &principal.username).await?,
    ))
}

pub(super) async fn create_session_space(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreateSessionSpaceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::create_space(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn update_session_space(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionSpaceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::update_space(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn delete_session_space(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<DeleteSessionSpaceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::delete_space(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn create_session_group(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<CreateSessionGroupReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::create_group(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn update_session_group(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSessionGroupReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::update_group(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn delete_session_group(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<DeleteSessionGroupReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::delete_group(&st.db, &principal.username, &id, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn reorder_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<ReorderSessionLayoutReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::reorder(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn move_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<MoveSessionsReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::move_sessions(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn restore_session_layout(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<RestoreSessionGroupsReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::restore_groups(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

pub(super) async fn set_session_group_preference(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(req): Json<SessionGroupPreferenceReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    session_layout::set_preference(&st.db, &principal.username, &id, req.collapsed)
        .await
        .map(Json)
        .map_err(|error| AppError::bad_request(error.to_string()))
}

pub(super) async fn set_session_placement_default(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SetSessionPlacementDefaultReq>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::set_default(&st.db, &principal.username, &req).await;
    mutation_response(&st, &principal.username, result).await
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteDefaultQuery {
    expected_revision: i64,
}

pub(super) async fn delete_session_placement_default(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((kind, value)): Path<(SessionPlacementSelectorKind, String)>,
    Query(query): Query<DeleteDefaultQuery>,
) -> ApiResult<Json<SessionLayoutView>> {
    require_human(&principal)?;
    let result = session_layout::delete_default(
        &st.db,
        &principal.username,
        kind,
        &value,
        query.expected_revision,
    )
    .await;
    mutation_response(&st, &principal.username, result).await
}

/// The `session_layout.events` operation — a fleet-global layout tail. The
/// browser still performs a normal read after an event; the event is only
/// invalidation, so reconnects and dropped messages cannot corrupt local
/// selection/disclosure state.
pub(super) async fn session_layout_events(
    State(st): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(input): Query<events::Input>,
) -> ApiResult<Sse<impl Stream<Item = Result<sse::Event, Infallible>>>> {
    // `actor = User` on the declaration is the same rule `require_human` was:
    // the layout is the signed-in operator's own dashboard state.
    super::encodings::authorized::<events::Events>(&st, &principal, input).await?;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(|result| {
        let event = result.ok()?;
        if event.kind != "session_layout" {
            return None;
        }
        Some(Ok(sse::Event::default()
            .event("session_layout")
            .json_data(&event.data)
            .unwrap_or_default()))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// Operation registry — `session_layout.*`, bound onto
// `weaver_api::operations::session_layout`. These are the operation-typed
// twins of every route above, including the per-item PATCH/DELETE and
// default-selector routes (`session_layout.events` is registered too, but as
// `io = Stream`, so `web::encodings` serves it). Every revision-guarded
// mutation still funnels through `mutation_response`/`mutation_result`, so
// the optimistic-concurrency conflict shape — refetch the current layout and
// hand it back alongside the 409 — is identical between the legacy routes and
// these operations. `session_layout.groups.preference.set` is the one
// exception, and was already an exception in the legacy handler: a collapse
// toggle carries no `expected_revision`, so there is nothing to conflict on.
// The legacy routes stay live and untouched until the coordinated route
// deletion pass.
// ---------------------------------------------------------------------------

/// The operation-typed twin of [`mutation_response`], returning the bare
/// output `register` expects instead of a `Json` wrapper.
async fn mutation_result(
    st: &AppState,
    username: &str,
    result: Result<SessionLayoutView, MutationError>,
) -> ApiResult<SessionLayoutView> {
    mutation_response(st, username, result)
        .await
        .map(|Json(layout)| layout)
}

pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<get::Get, _, _>(get_operation),
        register::<spaces::create::Create, _, _>(spaces_create_operation),
        register::<spaces::update::Update, _, _>(spaces_update_operation),
        register::<spaces::delete::Delete, _, _>(spaces_delete_operation),
        register::<groups::create::Create, _, _>(groups_create_operation),
        register::<groups::update::Update, _, _>(groups_update_operation),
        register::<groups::delete::Delete, _, _>(groups_delete_operation),
        register::<groups::preference::set::Set, _, _>(groups_preference_set_operation),
        register::<r#move::Move, _, _>(move_operation),
        register::<reorder::Reorder, _, _>(reorder_operation),
        register::<restore::Restore, _, _>(restore_operation),
        register::<defaults::set::Set, _, _>(defaults_set_operation),
        register::<defaults::delete::Delete, _, _>(defaults_delete_operation),
    ]
}

/// `session_layout.get` — the twin of [`get_session_layout`].
async fn get_operation(context: OperationContext, _input: get::Input) -> ApiResult<get::Output> {
    Ok(session_layout::get_layout(&context.state.db, &context.principal.username).await?)
}

/// `session_layout.spaces.create` — the twin of [`create_session_space`].
async fn spaces_create_operation(
    context: OperationContext,
    input: spaces::create::Input,
) -> ApiResult<spaces::create::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = CreateSessionSpaceReq {
        name: input.name,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::create_space(&st.db, &username, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.spaces.update` — the twin of [`update_session_space`].
async fn spaces_update_operation(
    context: OperationContext,
    input: spaces::update::Input,
) -> ApiResult<spaces::update::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = UpdateSessionSpaceReq {
        name: input.name,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::update_space(&st.db, &username, &input.id, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.spaces.delete` — the twin of [`delete_session_space`].
async fn spaces_delete_operation(
    context: OperationContext,
    input: spaces::delete::Input,
) -> ApiResult<spaces::delete::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = DeleteSessionSpaceReq {
        destination_group_id: input.destination_group_id,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::delete_space(&st.db, &username, &input.id, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.groups.create` — the twin of [`create_session_group`].
async fn groups_create_operation(
    context: OperationContext,
    input: groups::create::Input,
) -> ApiResult<groups::create::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = CreateSessionGroupReq {
        space_id: input.space_id,
        name: input.name,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::create_group(&st.db, &username, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.groups.update` — the twin of [`update_session_group`].
async fn groups_update_operation(
    context: OperationContext,
    input: groups::update::Input,
) -> ApiResult<groups::update::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = UpdateSessionGroupReq {
        name: input.name,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::update_group(&st.db, &username, &input.id, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.groups.delete` — the twin of [`delete_session_group`].
async fn groups_delete_operation(
    context: OperationContext,
    input: groups::delete::Input,
) -> ApiResult<groups::delete::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = DeleteSessionGroupReq {
        destination_group_id: input.destination_group_id,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::delete_group(&st.db, &username, &input.id, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.groups.preference.set` — the twin of
/// [`set_session_group_preference`]. Unlike the rest of this bundle it does
/// not go through `mutation_result`: the legacy route never did either,
/// because a collapse toggle carries no `expected_revision` to conflict on.
async fn groups_preference_set_operation(
    context: OperationContext,
    input: groups::preference::set::Input,
) -> ApiResult<groups::preference::set::Output> {
    session_layout::set_preference(
        &context.state.db,
        &context.principal.username,
        &input.id,
        input.collapsed,
    )
    .await
    .map_err(|error| AppError::bad_request(error.to_string()))
}

/// `session_layout.move` — the twin of [`move_session_layout`].
async fn move_operation(
    context: OperationContext,
    input: r#move::Input,
) -> ApiResult<r#move::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = MoveSessionsReq {
        session_ids: input.session_ids,
        destination_group_id: input.destination_group_id,
        before_session_id: input.before_session_id,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::move_sessions(&st.db, &username, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.reorder` — the twin of [`reorder_session_layout`].
async fn reorder_operation(
    context: OperationContext,
    input: reorder::Input,
) -> ApiResult<reorder::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = ReorderSessionLayoutReq {
        kind: input.kind,
        id: input.id,
        before_id: input.before_id,
        destination_space_id: input.destination_space_id,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::reorder(&st.db, &username, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.restore` — the twin of [`restore_session_layout`].
async fn restore_operation(
    context: OperationContext,
    input: restore::Input,
) -> ApiResult<restore::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = RestoreSessionGroupsReq {
        groups: input.groups,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::restore_groups(&st.db, &username, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.defaults.set` — the twin of
/// [`set_session_placement_default`].
async fn defaults_set_operation(
    context: OperationContext,
    input: defaults::set::Input,
) -> ApiResult<defaults::set::Output> {
    let st = context.state;
    let username = context.principal.username;
    let req = SetSessionPlacementDefaultReq {
        selector_kind: input.selector_kind,
        selector_value: input.selector_value,
        group_id: input.group_id,
        expected_revision: input.expected_revision,
    };
    let result = session_layout::set_default(&st.db, &username, &req).await;
    mutation_result(&st, &username, result).await
}

/// `session_layout.defaults.delete` — the twin of
/// [`delete_session_placement_default`].
async fn defaults_delete_operation(
    context: OperationContext,
    input: defaults::delete::Input,
) -> ApiResult<defaults::delete::Output> {
    let st = context.state;
    let username = context.principal.username;
    let result = session_layout::delete_default(
        &st.db,
        &username,
        input.selector_kind,
        &input.selector_value,
        input.expected_revision,
    )
    .await;
    mutation_result(&st, &username, result).await
}
