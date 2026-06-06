//! loom — the optional orchestrator binary that sits on top of `weaver-core`.
//!
//! Loom owns sessions (one tmux + one running agent per branch), the REST API,
//! the Vue web UI, the monitor loop, and recently-used repository bookkeeping.
//! The agent-facing `weaver` CLI does not depend on loom; running loom is
//! purely additive.

pub mod agent;
pub mod client;
pub mod db;
pub mod endpoint;
pub mod github;
pub mod monitor;
pub mod repo;
pub mod server;
pub mod session;
pub mod terminal;
pub mod tmux;
pub mod web;

pub use web::AppState;

// Re-export weaver-core modules at the same paths so the orchestrator code can
// continue to use short `crate::events`/`crate::git`/etc. references.
pub use weaver_core::db::Db;
pub use weaver_core::{branch, config, events, git, issue};
