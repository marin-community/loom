//! Session lifecycle, status projection, and normalized history.
//!
//! One file per operation, mirroring the `issues` bundle. `self.get` keeps its
//! historical id — a caller-facing bootstrap read — but lives in this bundle;
//! its module is named `context` rather than `self` because `self` cannot
//! be a module name, and it carries an explicit `bundle = "sessions"` so the
//! id's own `self` prefix does not get inferred as its bundle.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod adopt;
pub mod archive;
pub mod changes;
pub mod chat;
pub mod context;
pub mod conversation;
pub mod events;
pub mod files;
pub mod get;
pub mod handoff;
pub mod history;
pub mod ide_info;
pub mod interrupt;
pub mod launch;
pub mod launches;
pub mod list;
pub mod mode;
pub mod preview;
pub mod raw;
pub mod recover;
pub mod scratch;
pub mod send;
pub mod shells;
pub mod status;
pub mod summary;
pub mod tags;
pub mod url;

static OPERATIONS: &[&OperationSpec] = &[
    <context::Get as Operation>::SPEC,
    <summary::get::Get as Operation>::SPEC,
    <list::List as Operation>::SPEC,
    <get::Get as Operation>::SPEC,
    <launch::Launch as Operation>::SPEC,
    <launches::resolve::Resolve as Operation>::SPEC,
    <send::Send as Operation>::SPEC,
    <interrupt::Interrupt as Operation>::SPEC,
    <preview::Preview as Operation>::SPEC,
    <events::list::List as Operation>::SPEC,
    <events::create::Create as Operation>::SPEC,
    <history::list::List as Operation>::SPEC,
    <history::search::Search as Operation>::SPEC,
    <status::get::Get as Operation>::SPEC,
    <status::set::Set as Operation>::SPEC,
    <tags::list::List as Operation>::SPEC,
    <tags::set::Set as Operation>::SPEC,
    <tags::delete::Delete as Operation>::SPEC,
    <adopt::Adopt as Operation>::SPEC,
    <archive::Archive as Operation>::SPEC,
    <recover::Recover as Operation>::SPEC,
    <handoff::Handoff as Operation>::SPEC,
    <changes::Changes as Operation>::SPEC,
    <chat::Chat as Operation>::SPEC,
    <conversation::Conversation as Operation>::SPEC,
    <files::Files as Operation>::SPEC,
    <mode::Mode as Operation>::SPEC,
    <raw::Raw as Operation>::SPEC,
    <url::Url as Operation>::SPEC,
    <ide_info::IdeInfo as Operation>::SPEC,
    <shells::list::List as Operation>::SPEC,
    <scratch::limits::Limits as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "sessions",
        label: "Session workflow",
        operations: OPERATIONS,
    }
}
