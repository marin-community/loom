//! Identity, credentials, and fleet-wide access administration.
//!
//! Previously excluded from the registry entirely: the old design treated
//! "administrative" as a reason to leave a route out rather than a fact to
//! record on it. Every operation here is `actor = User` (a human managing
//! their own identity — signing in, minting a personal token, setting a
//! password) or `actor = Admin` (fleet administration — approving operators,
//! configuring GitHub sign-in, registering federation mappings, minting
//! automation tokens), except `auth.federate`, which is `actor = Anonymous`
//! because its caller is a CI system exchanging a workload-identity token,
//! never a human and never an agent session. Because an MCP projection is
//! rejected on any operation that is not `SessionSelf` (see
//! `validate_operation_registry`), none of this surface can acquire an MCP
//! tool by accident — the human-only, operator-only, and automation-only
//! boundaries are enforced properties of these declarations, not an absence.
//!
//! `GET /auth/github/login` and `GET /auth/github/callback` are deliberately
//! NOT registered here: they are browser redirects (a 302 plus a `Set-Cookie`
//! header, never a JSON body), so they have no `Io` this registry can
//! describe, and nothing but an `<a href>` ever calls them. `loom login` /
//! `loom logout` (the CLI) and `loom context` (local client contexts) are
//! Tier C — see `auth.login`'s doc comment — and stay hand-written.
//!
//! One file per operation, exactly like `issues`. Dotted ids become
//! subdirectories: `auth.users.create` lives at `auth/users/create.rs`.

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
