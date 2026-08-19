//! Access discovery and human-approved external credential expansion.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod effective;
pub mod explain;
pub mod github;
pub mod requests;

/// Maximum byte length of a permission-request reason.
pub const MAX_REASON_LEN: usize = 4_096;

static OPERATIONS: &[&OperationSpec] = &[
    <effective::get::Get as Operation>::SPEC,
    <explain::Explain as Operation>::SPEC,
    <requests::list::List as Operation>::SPEC,
    <requests::create::Create as Operation>::SPEC,
    <requests::approve::Approve as Operation>::SPEC,
    <requests::deny::Deny as Operation>::SPEC,
    <github::grant::Grant as Operation>::SPEC,
    <github::revoke::Revoke as Operation>::SPEC,
    <github::token::Token as Operation>::SPEC,
    <github::restricted::invoke::Invoke as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "permissions",
        label: "Access and approvals",
        operations: OPERATIONS,
    }
}
