//! Loom's command line, assembled from the operation registry.
//!
//! Registered operations contribute no code here beyond one `bind::<Op>()` line.
//! Host-local commands — `server run`, `setup`, `completions`, the server-free
//! half of `config` — are not operations and keep their own clap definitions:
//! they never reach the API, so there is nothing for the registry to own.

pub mod dispatch;

pub use dispatch::{augment, bind, resolve, CliBinding};

use weaver_api::operations::issues;

/// Every registered operation's command-line binding.
///
/// One line per operation. A registered operation missing from this list fails
/// the parity test rather than silently vanishing from the CLI.
pub fn bindings() -> Vec<CliBinding> {
    vec![
        bind::<issues::list::List>(),
        bind::<issues::get::Get>(),
        bind::<issues::create::Create>(),
        bind::<issues::backlog::create::Create>(),
        bind::<issues::close::Close>(),
        bind::<issues::reopen::Reopen>(),
        bind::<issues::delete::Delete>(),
        bind::<issues::tags::set::Set>(),
        bind::<issues::tags::delete::Delete>(),
        bind::<issues::actions::Actions>(),
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
        assert!(missing.is_empty(), "operations advertise a CLI but have no binding: {missing:?}");
    }
}
