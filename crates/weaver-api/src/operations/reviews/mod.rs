//! Creator-private draft feedback that, once submitted, becomes a durable
//! review delivered into the reviewed session's own conversation.
//!
//! One file per operation.

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
