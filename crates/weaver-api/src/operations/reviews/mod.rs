//! Creator-private draft feedback that, once submitted, becomes a durable
//! review delivered into the reviewed session's own conversation.
//!
//! `reviews.list` and `reviews.create` are keyed on a session — the legacy
//! route is `GET`/`POST /sessions/{id}/reviews`, since a review always begins
//! from one session's artifact or change-set — so `session` is their
//! `#[operand(context)]` operand where the actor policy allows it
//! (`reviews.list`; `reviews.create` is operator-only and names its target
//! session as an ordinary operand instead, see that file). They still live in
//! this bundle rather than under `sessions.*`: every other operation here
//! acts on a review that already exists, by its own id, and one id prefix per
//! resource keeps the whole thing in one file tree instead of splitting it
//! across two. One file per operation. Adding a new one means adding its file
//! here and its handler in the mirrored server tree — no clap variant, no
//! client wrapper, no MCP schema, no capability set, unless asked for.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod comments;
pub mod create;
pub mod discard;
pub mod get;
pub mod list;
pub mod retarget;
pub mod retry_delivery;
pub mod submit;
pub mod update;

static OPERATIONS: &[&OperationSpec] = &[
    <get::Get as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <discard::Discard as Operation>::SPEC,
    <retarget::Retarget as Operation>::SPEC,
    <list::List as Operation>::SPEC,
    <create::Create as Operation>::SPEC,
    <comments::create::Create as Operation>::SPEC,
    <comments::update::Update as Operation>::SPEC,
    <comments::delete::Delete as Operation>::SPEC,
    <comments::resolve::Resolve as Operation>::SPEC,
    <submit::Submit as Operation>::SPEC,
    <retry_delivery::RetryDelivery as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "reviews",
        label: "Reviews",
        operations: OPERATIONS,
    }
}
