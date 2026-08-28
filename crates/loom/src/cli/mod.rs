//! Loom's command line, assembled from the operation registry.
//!
//! Registered operations contribute no code here beyond one `bind::<Op>()` line.
//! Host-local commands — `server run`, `setup`, `completions`, the server-free
//! half of `config` — are not operations and keep their own clap definitions:
//! they never reach the API, so there is nothing for the registry to own.

pub mod agent;
pub mod clap_bind;
pub mod commands;
pub mod dispatch;
pub mod host;
pub mod support;

pub use dispatch::{augment, bind, resolve, CliBinding};

use weaver_api::operations::*;

/// Every registered operation's command-line binding.
///
/// One line per operation. A registered operation missing from this list fails
/// the parity test.
pub fn bindings() -> Vec<CliBinding> {
    vec![
        bind::<agents::custom::create::Op>(),
        bind::<agents::custom::delete::Op>(),
        bind::<agents::custom::update::Op>(),
        bind::<agents::list::Op>(),
        bind::<artifacts::delete::Op>(),
        bind::<artifacts::get::Op>(),
        bind::<artifacts::history::Op>(),
        bind::<artifacts::list::Op>(),
        bind::<artifacts::threads::comment::Op>(),
        bind::<artifacts::threads::list::Op>(),
        bind::<artifacts::threads::resolve::Op>(),
        bind::<artifacts::write::Op>(),
        bind::<auth::automation_token::Op>(),
        bind::<auth::federate::Op>(),
        bind::<auth::federations::create::Op>(),
        bind::<auth::federations::list::Op>(),
        bind::<auth::federations::remove::Op>(),
        bind::<auth::github_config::get::Op>(),
        bind::<auth::github_config::set::Op>(),
        bind::<auth::github_token::get::Op>(),
        bind::<auth::github_token::remove::Op>(),
        bind::<auth::github_token::set::Op>(),
        bind::<auth::me::Op>(),
        bind::<auth::set_password::Op>(),
        bind::<auth::tokens::create::Op>(),
        bind::<auth::tokens::list::Op>(),
        bind::<auth::tokens::revoke::Op>(),
        bind::<auth::users::create::Op>(),
        bind::<auth::users::list::Op>(),
        bind::<auth::users::remove::Op>(),
        bind::<auth::users::set_role::Op>(),
        bind::<branches::events::create::Op>(),
        bind::<branches::events::list::Op>(),
        bind::<branches::get::Op>(),
        bind::<branches::issues::list::Op>(),
        bind::<branches::list::Op>(),
        bind::<branches::slack::send::Op>(),
        bind::<branches::status::set::Op>(),
        bind::<branches::tags::delete::Op>(),
        bind::<branches::tags::set::Op>(),
        bind::<branches::update::Op>(),
        bind::<channels::archive::Op>(),
        bind::<channels::create::Op>(),
        bind::<channels::get::Op>(),
        bind::<channels::list::Op>(),
        bind::<channels::messages::create::Op>(),
        bind::<channels::messages::list::Op>(),
        bind::<channels::read_marker::set::Op>(),
        bind::<channels::subscription::set::Op>(),
        bind::<channels::wait::Op>(),
        bind::<deployment::reconcile::Op>(),
        bind::<issues::actions::Op>(),
        bind::<issues::backlog::create::Op>(),
        bind::<issues::board::Op>(),
        bind::<issues::close::Op>(),
        bind::<issues::create::Op>(),
        bind::<issues::delete::Op>(),
        bind::<issues::get::Op>(),
        bind::<issues::list::Op>(),
        bind::<issues::reopen::Op>(),
        bind::<issues::tags::delete::Op>(),
        bind::<issues::tags::set::Op>(),
        bind::<issues::update::Op>(),
        bind::<mcps::custom::create::Op>(),
        bind::<mcps::custom::delete::Op>(),
        bind::<mcps::custom::get::Op>(),
        bind::<mcps::custom::list::Op>(),
        bind::<mcps::custom::update::Op>(),
        bind::<mcps::get::Op>(),
        bind::<permissions::effective::get::Op>(),
        bind::<permissions::explain::Op>(),
        bind::<permissions::github::grant::Op>(),
        bind::<permissions::github::revoke::Op>(),
        bind::<permissions::github::token::Op>(),
        bind::<permissions::requests::approve::Op>(),
        bind::<permissions::requests::create::Op>(),
        bind::<permissions::requests::deny::Op>(),
        bind::<permissions::requests::list::Op>(),
        bind::<profiles::clone::Op>(),
        bind::<profiles::create::Op>(),
        bind::<profiles::delete::Op>(),
        bind::<profiles::effective::Op>(),
        bind::<profiles::env::delete::Op>(),
        bind::<profiles::env::set::Op>(),
        bind::<profiles::get::Op>(),
        bind::<profiles::list::Op>(),
        bind::<profiles::update::Op>(),
        bind::<repos::branches::Op>(),
        bind::<repos::env::delete::Op>(),
        bind::<repos::env::get::Op>(),
        bind::<repos::env::set::Op>(),
        bind::<repos::list::Op>(),
        bind::<repos::recent::Op>(),
        bind::<repos::register::Op>(),
        bind::<repos::revisions::validate::Op>(),
        bind::<reviews::comments::delete::Op>(),
        bind::<reviews::discard::Op>(),
        bind::<reviews::get::Op>(),
        bind::<reviews::retarget::Op>(),
        bind::<reviews::retry_delivery::Op>(),
        bind::<runs::create::Op>(),
        bind::<runs::get::Op>(),
        bind::<runs::list::Op>(),
        bind::<session_layout::defaults::delete::Op>(),
        bind::<session_layout::defaults::set::Op>(),
        bind::<session_layout::get::Op>(),
        bind::<session_layout::groups::create::Op>(),
        bind::<session_layout::groups::delete::Op>(),
        bind::<session_layout::groups::update::Op>(),
        bind::<session_layout::r#move::Op>(),
        bind::<session_layout::reorder::Op>(),
        bind::<session_layout::restore::Op>(),
        bind::<session_layout::spaces::create::Op>(),
        bind::<session_layout::spaces::delete::Op>(),
        bind::<session_layout::spaces::update::Op>(),
        bind::<sessions::adopt::Op>(),
        bind::<sessions::archive::Op>(),
        bind::<sessions::changes::Op>(),
        bind::<sessions::chat::Op>(),
        bind::<sessions::conversation::Op>(),
        bind::<sessions::events::create::Op>(),
        bind::<sessions::events::list::Op>(),
        bind::<sessions::files::Op>(),
        bind::<sessions::get::Op>(),
        bind::<sessions::github::access::list::Op>(),
        bind::<sessions::handoff::Op>(),
        bind::<sessions::history::list::Op>(),
        bind::<sessions::ide_info::Op>(),
        bind::<sessions::interrupt::Op>(),
        bind::<sessions::launch::Op>(),
        bind::<sessions::list::Op>(),
        bind::<sessions::mode::Op>(),
        bind::<sessions::preview::Op>(),
        bind::<sessions::recover::Op>(),
        bind::<sessions::scratch::delete::Op>(),
        bind::<sessions::scratch::limits::Op>(),
        bind::<sessions::scratch::list::Op>(),
        bind::<sessions::send::Op>(),
        bind::<sessions::shells::list::Op>(),
        bind::<sessions::status::get::Op>(),
        bind::<sessions::status::set::Op>(),
        bind::<sessions::summary::get::Op>(),
        bind::<sessions::summary::list::Op>(),
        bind::<sessions::tags::delete::Op>(),
        bind::<sessions::tags::list::Op>(),
        bind::<sessions::tags::replace::Op>(),
        bind::<sessions::tags::set::Op>(),
        bind::<sessions::url::Op>(),
        bind::<settings::env::delete::Op>(),
        bind::<settings::env::list::Op>(),
        bind::<settings::env::set::Op>(),
        bind::<settings::get::Op>(),
        bind::<settings::patch::Op>(),
        bind::<shell::restart::Op>(),
        bind::<tasks::list::Op>(),
        bind::<watches::create::Op>(),
        bind::<watches::delete::Op>(),
        bind::<watches::get::Op>(),
        bind::<watches::list::Op>(),
        bind::<watches::run::Op>(),
        bind::<watches::runs::Op>(),
        bind::<watches::update::Op>(),
    ]
}

#[cfg(test)]
mod tests {
    /// Every registered JSON operation must be reachable from the CLI unless it
    /// deliberately declares no CLI command.
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

    /// And nothing is bound that no command can reach.
    ///
    /// A binding whose operation declares `cli = -` is never placed in the tree
    /// — `generic_bindings` filters it out — so keeping it in `bindings()`
    /// serves no purpose.
    #[test]
    fn every_binding_names_an_operation_with_a_command() {
        let stranded: Vec<_> = super::bindings()
            .iter()
            .filter(|binding| binding.operation.cli.is_none())
            .map(|binding| binding.operation.id)
            .collect();
        assert!(
            stranded.is_empty(),
            "bound operations declare no CLI, so nothing reaches them: {stranded:?}"
        );
    }
}
