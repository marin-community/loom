//! loom — the optional orchestrator binary that sits on top of `weaver-core`.
//!
//! Loom owns sessions (one terminal supervisor + one running agent per branch),
//! the REST API, the Vue web UI, the monitor loop, and recently-used repository
//! bookkeeping. The agent-facing `weaver` CLI does not depend on loom; running
//! loom is purely additive.

pub mod acp;
pub mod agent;
pub mod agent_env;
pub mod auth;
pub mod automation;
pub mod backend;
pub mod builtins;
pub mod chat;
pub mod chatlog;
pub mod client;
pub mod client_context;
pub mod custom_agents;
pub mod custom_mcp;
pub mod db;
pub mod endpoint;
pub mod envfile;
pub mod github;
pub mod github_app;
pub mod github_manifest;
pub mod github_trigger;
pub mod ide;
pub mod launch_gate;
pub mod logs;
pub mod loom_config;
pub mod mcp;
pub mod monitor;
pub mod profile;
pub mod repo;
pub mod repo_env;
pub mod runner;
pub mod runs;
pub(crate) mod runtime;
pub mod server;
pub mod session;
pub mod setup;
pub mod shell;
pub mod slack;
pub mod tasks;
pub mod terminal;
pub mod user_token;
pub mod watch;
pub mod web;

pub use web::AppState;

// Re-export weaver-core modules at the same paths so the orchestrator code can
// continue to use short `crate::events`/`crate::git`/etc. references.
pub use weaver_core::db::Db;
pub use weaver_core::{branch, config, events, git, issue};
