//! Identity, credentials, and fleet-wide access administration.
//!
//! Operations use `actor = User` for identity self-management (sign-in,
//! token creation, password changes), `actor = Admin` for fleet administration
//! (user approval, GitHub sign-in configuration, federation mappings, automation
//! tokens), and `actor = Anonymous` for `auth.federate` (CI systems exchanging
//! workload-identity tokens).
//!
//! Browser OAuth endpoints (`GET /auth/github/login`, `GET /auth/github/callback`)
//! are not registered here: they return redirects, not JSON, and are never called
//! programmatically.
//!
//! One file per operation. Dotted ids become subdirectories:
//! `auth.users.create` lives at `auth/users/create.rs`.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod automation_token;
pub mod federate;
pub mod federations;
pub mod github_config;
pub mod github_token;
pub mod login;
pub mod logout;
pub mod me;
pub mod set_password;
pub mod tokens;
pub mod users;

static OPERATIONS: &[&OperationSpec] = &[
    <me::Me as Operation>::SPEC,
    <login::Login as Operation>::SPEC,
    <logout::Logout as Operation>::SPEC,
    <federate::Federate as Operation>::SPEC,
    <automation_token::AutomationToken as Operation>::SPEC,
    <set_password::SetPassword as Operation>::SPEC,
    <tokens::list::List as Operation>::SPEC,
    <tokens::create::Create as Operation>::SPEC,
    <tokens::revoke::Revoke as Operation>::SPEC,
    <federations::list::List as Operation>::SPEC,
    <federations::create::Create as Operation>::SPEC,
    <federations::remove::Remove as Operation>::SPEC,
    <users::list::List as Operation>::SPEC,
    <users::create::Create as Operation>::SPEC,
    <users::set_role::SetRole as Operation>::SPEC,
    <users::remove::Remove as Operation>::SPEC,
    <github_token::get::Get as Operation>::SPEC,
    <github_token::set::Set as Operation>::SPEC,
    <github_token::remove::Remove as Operation>::SPEC,
    <github_config::get::Get as Operation>::SPEC,
    <github_config::set::Set as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "auth",
        label: "Authentication and access",
        operations: OPERATIONS,
    }
}
