//! Creator-private draft feedback that, once submitted, becomes a durable
//! review delivered into the reviewed session's own conversation.
//!
//! `reviews.list` and `reviews.create` are scoped under a session
//! (`sessions.reviews.*`) rather than here, since a review always begins from
//! one session's artifact or change-set; the operations in this bundle act on
//! a review that already exists, by its own id. One file per operation.
//! Adding `reviews.discard` means adding its file here and its handler in the
//! mirrored server tree — no clap variant, no client wrapper, no MCP schema,
//! no capability set.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod comments;
pub mod retry_delivery;
pub mod submit;

static OPERATIONS: &[&OperationSpec] = &[
    <comments::create::Create as Operation>::SPEC,
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
