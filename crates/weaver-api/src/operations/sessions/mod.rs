//! Session lifecycle, status projection, and normalized history.
//!
//! One file per operation, mirroring the `issues` bundle. The bootstrap read
//! operation (`self.get`) lives in this bundle; its module is named `context`
//! rather than `self` because `self` is reserved, and it includes an explicit
//! `bundle = "sessions"` declaration to prevent the id prefix from being inferred.

use super::registry::{Operation, OperationSpec};
use super::OperationBundle;

pub(super) use super::prelude;

pub mod adopt;
pub mod archive;
pub mod changes;
pub mod chat;
pub mod config;
pub mod context;
pub mod conversation;
pub mod delete;
pub mod events;
pub mod files;
pub mod get;
pub mod github;
pub mod handoff;
pub mod history;
pub mod ide_info;
pub mod interrupt;
pub mod launch;
pub mod launches;
pub mod list;
pub mod mode;
pub mod permissions;
pub mod preview;
pub mod prompt;
pub mod raw;
pub mod recover;
pub mod resumption_cue;
pub mod scratch;
pub mod send;
pub mod shells;
pub mod status;
pub mod summary;
pub mod tags;
pub mod terminal;
pub mod title;
pub mod update;
pub mod url;

static OPERATIONS: &[&OperationSpec] = &[
    <context::Get as Operation>::SPEC,
    <summary::get::Get as Operation>::SPEC,
    <summary::list::List as Operation>::SPEC,
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
    <tags::replace::Replace as Operation>::SPEC,
    <tags::delete::Delete as Operation>::SPEC,
    <adopt::Adopt as Operation>::SPEC,
    <archive::Archive as Operation>::SPEC,
    <recover::Recover as Operation>::SPEC,
    <handoff::Handoff as Operation>::SPEC,
    <handoff::resolve::Resolve as Operation>::SPEC,
    <changes::Changes as Operation>::SPEC,
    <chat::Chat as Operation>::SPEC,
    <conversation::Conversation as Operation>::SPEC,
    <conversation::block::Block as Operation>::SPEC,
    <files::Files as Operation>::SPEC,
    <mode::Mode as Operation>::SPEC,
    <raw::Raw as Operation>::SPEC,
    <url::Url as Operation>::SPEC,
    <ide_info::IdeInfo as Operation>::SPEC,
    <shells::list::List as Operation>::SPEC,
    <shells::delete::Delete as Operation>::SPEC,
    <scratch::limits::Limits as Operation>::SPEC,
    <scratch::list::List as Operation>::SPEC,
    <scratch::write::Write as Operation>::SPEC,
    <scratch::delete::Delete as Operation>::SPEC,
    <update::Update as Operation>::SPEC,
    <delete::Delete as Operation>::SPEC,
    <config::set::Set as Operation>::SPEC,
    <github::refresh::Refresh as Operation>::SPEC,
    <github::set::Set as Operation>::SPEC,
    <github::clear::Clear as Operation>::SPEC,
    <github::access::list::List as Operation>::SPEC,
    <github::labels::add::Add as Operation>::SPEC,
    <prompt::create::Create as Operation>::SPEC,
    <prompt::retract::Retract as Operation>::SPEC,
    <resumption_cue::get::Get as Operation>::SPEC,
    <resumption_cue::ensure::Ensure as Operation>::SPEC,
    <permissions::answer::Answer as Operation>::SPEC,
    <title::regenerate::Regenerate as Operation>::SPEC,
    <title::generation::set::Set as Operation>::SPEC,
    // Non-JSON: SSE feeds and terminal websockets. Registered exactly like the
    // rest — only the response encoding differs, so a custom handler in
    // `loom::web::encodings` serves them off these same declarations.
    <events::stream::Stream as Operation>::SPEC,
    <chat::stream::Stream as Operation>::SPEC,
    <terminal::Terminal as Operation>::SPEC,
    <shells::terminal::Terminal as Operation>::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "sessions",
        label: "Session workflow",
        operations: OPERATIONS,
    }
}
