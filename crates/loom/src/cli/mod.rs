//! Loom's command line, assembled from the operation registry.
//!
//! Registered operations contribute no code here beyond one `bind::<Op>()` line.
//! Host-local commands — `server run`, `setup`, `completions`, the server-free
//! half of `config` — are not operations and keep their own clap definitions:
//! they never reach the API, so there is nothing for the registry to own.

pub mod dispatch;

pub use dispatch::{augment, bind, resolve, CliBinding};

use weaver_api::operations::*;

/// Every registered operation's command-line binding.
///
/// One line per operation. A registered operation missing from this list fails
/// the parity test rather than silently vanishing from the CLI.
pub fn bindings() -> Vec<CliBinding> {
    vec![
        bind::<agents::custom::create::Create>(),
        bind::<agents::custom::delete::Delete>(),
        bind::<agents::custom::update::Update>(),
        bind::<agents::list::List>(),
        bind::<artifacts::delete::Delete>(),
        bind::<artifacts::get::Get>(),
        bind::<artifacts::history::History>(),
        bind::<artifacts::list::List>(),
        bind::<artifacts::threads::comment::Comment>(),
        bind::<artifacts::threads::list::List>(),
        bind::<artifacts::threads::resolve::Resolve>(),
        bind::<artifacts::write::Write>(),
        bind::<auth::automation_token::AutomationToken>(),
        bind::<auth::federations::create::Create>(),
        bind::<auth::federations::list::List>(),
        bind::<auth::federations::remove::Remove>(),
        bind::<auth::github_config::get::Get>(),
        bind::<auth::github_config::set::Set>(),
        bind::<auth::github_token::get::Get>(),
        bind::<auth::github_token::remove::Remove>(),
        bind::<auth::github_token::set::Set>(),
        bind::<auth::me::Me>(),
        bind::<auth::set_password::SetPassword>(),
        bind::<auth::tokens::create::Create>(),
        bind::<auth::tokens::list::List>(),
        bind::<auth::tokens::revoke::Revoke>(),
        bind::<auth::users::create::Create>(),
        bind::<auth::users::list::List>(),
        bind::<auth::users::remove::Remove>(),
        bind::<auth::users::set_role::SetRole>(),
        bind::<branches::events::create::Create>(),
        bind::<branches::events::list::List>(),
        bind::<branches::get::Get>(),
        bind::<branches::issues::list::List>(),
        bind::<branches::list::List>(),
        bind::<branches::slack::reply::Reply>(),
        bind::<branches::status::set::Set>(),
        bind::<branches::tags::delete::Delete>(),
        bind::<branches::tags::set::Set>(),
        bind::<branches::update::Update>(),
        bind::<channels::create::Create>(),
        bind::<channels::get::Get>(),
        bind::<channels::list::List>(),
        bind::<channels::messages::create::Create>(),
        bind::<channels::messages::list::List>(),
        bind::<channels::read_marker::set::Set>(),
        bind::<channels::subscription::set::Set>(),
        bind::<channels::wait::Wait>(),
        bind::<deployment::reconcile::Reconcile>(),
        bind::<issues::actions::Actions>(),
        bind::<issues::backlog::create::Create>(),
        bind::<issues::close::Close>(),
        bind::<issues::create::Create>(),
        bind::<issues::delete::Delete>(),
        bind::<issues::get::Get>(),
        bind::<issues::list::List>(),
        bind::<issues::reopen::Reopen>(),
        bind::<issues::tags::delete::Delete>(),
        bind::<issues::tags::set::Set>(),
        bind::<mcps::custom::create::Create>(),
        bind::<mcps::custom::delete::Delete>(),
        bind::<mcps::custom::get::Get>(),
        bind::<mcps::custom::list::List>(),
        bind::<mcps::custom::update::Update>(),
        bind::<mcps::get::Get>(),
        bind::<permissions::effective::get::Get>(),
        bind::<permissions::explain::Explain>(),
        bind::<permissions::github::grant::Grant>(),
        bind::<permissions::github::revoke::Revoke>(),
        bind::<permissions::github::token::Token>(),
        bind::<permissions::requests::approve::Approve>(),
        bind::<permissions::requests::create::Create>(),
        bind::<permissions::requests::deny::Deny>(),
        bind::<permissions::requests::list::List>(),
        bind::<profiles::clone::Clone>(),
        bind::<profiles::create::Create>(),
        bind::<profiles::delete::Delete>(),
        bind::<profiles::effective::Effective>(),
        bind::<profiles::env::delete::Delete>(),
        bind::<profiles::env::set::Set>(),
        bind::<profiles::get::Get>(),
        bind::<profiles::list::List>(),
        bind::<profiles::probe::Probe>(),
        bind::<profiles::update::Update>(),
        bind::<repos::branches::List>(),
        bind::<repos::env::delete::Delete>(),
        bind::<repos::env::get::Get>(),
        bind::<repos::env::set::Set>(),
        bind::<repos::list::List>(),
        bind::<repos::recent::Recent>(),
        bind::<repos::register::Register>(),
        bind::<repos::revisions::validate::Validate>(),
        bind::<runs::get::Get>(),
        bind::<runs::list::List>(),
        bind::<sessions::adopt::Adopt>(),
        bind::<sessions::archive::Archive>(),
        bind::<sessions::changes::Changes>(),
        bind::<sessions::chat::Chat>(),
        bind::<sessions::context::Get>(),
        bind::<sessions::conversation::Conversation>(),
        bind::<sessions::events::create::Create>(),
        bind::<sessions::events::list::List>(),
        bind::<sessions::files::Files>(),
        bind::<sessions::get::Get>(),
        bind::<sessions::handoff::Handoff>(),
        bind::<sessions::ide_info::IdeInfo>(),
        bind::<sessions::interrupt::Interrupt>(),
        bind::<sessions::launch::Launch>(),
        bind::<sessions::list::List>(),
        bind::<sessions::mode::Mode>(),
        bind::<sessions::preview::Preview>(),
        bind::<sessions::raw::Raw>(),
        bind::<sessions::recover::Recover>(),
        bind::<sessions::scratch::limits::Limits>(),
        bind::<sessions::send::Send>(),
        bind::<sessions::shells::list::List>(),
        bind::<sessions::status::get::Get>(),
        bind::<sessions::status::set::Set>(),
        bind::<sessions::summary::get::Get>(),
        bind::<sessions::tags::delete::Delete>(),
        bind::<sessions::tags::list::List>(),
        bind::<sessions::tags::set::Set>(),
        bind::<sessions::url::Url>(),
        bind::<settings::env::delete::Delete>(),
        bind::<settings::env::list::List>(),
        bind::<settings::env::set::Set>(),
        bind::<settings::get::Get>(),
        bind::<settings::patch::Patch>(),
        bind::<tasks::list::List>(),
        bind::<watches::create::Create>(),
        bind::<watches::delete::Delete>(),
        bind::<watches::get::Get>(),
        bind::<watches::list::List>(),
        bind::<watches::programs::Programs>(),
        bind::<watches::run::Run>(),
        bind::<watches::runs::Runs>(),
        bind::<watches::update::Update>(),
    ]
}

#[cfg(test)]
mod tests {
    /// Every registered JSON operation must be reachable from the CLI unless it
    /// deliberately declares no projection.
    #[test]
    fn registered_operations_have_a_binding() {
        let bound: Vec<_> = super::bindings()
            .iter()
            .map(|binding| binding.operation.id)
            .collect();
        let missing: Vec<_> = weaver_api::operations()
            .filter(|operation| operation.cli.is_some())
            .map(|operation| operation.id)
            .filter(|id| !bound.contains(id))
            .collect();
        assert!(
            missing.is_empty(),
            "operations advertise a CLI but have no binding: {missing:?}"
        );
    }
}
