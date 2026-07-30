//! Loom's persistent state and the rules over it.
//!
//! What a session, profile, agent or conversation *is*, and what may legally
//! happen to one. Everything here reaches storage and the event bus through
//! [`Ctx`] and stops there: no process spawning, no network calls, no live
//! process registries. That boundary is the point — it is what makes this layer
//! testable against an in-memory database.

pub mod acp;
pub mod agent;
pub mod agent_env;
pub mod auth;
pub mod automation;
pub mod channels;
pub mod chat;
pub mod chatlog;
pub mod custom_agents;
pub mod custom_mcp;
pub mod db;
pub mod history;
pub mod mcp;
pub mod profile;
pub mod repo_env;
pub mod review_inbox;
pub mod runs;
pub mod session;
pub mod session_layout;
pub mod session_manager;
pub mod shell;
pub mod status;

// The layer below, re-exported so `crate::backend` and friends resolve here and
// so dependents can reach the whole stack through one crate.
pub use loom_ctx::Ctx;
pub use loom_ctx::{
    backend, changes, client_context, ctx, envfile, launch_gate, links, logs, loom_config, runner,
    scratch,
};

// Crate-local aliases keep implementation imports short without exposing
// weaver-core's storage and domain modules as part of the public API.
pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{config, events};
