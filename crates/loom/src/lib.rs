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
pub(crate) mod changes;
pub mod chat;
pub(crate) mod chatlog;
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
pub(crate) mod handoff;
pub(crate) mod history;
pub mod ide;
pub(crate) mod launch;
pub mod launch_gate;
pub mod logs;
pub mod loom_config;
pub mod mcp;
pub mod metadata_assist;
pub mod monitor;
pub mod profile;
pub(crate) mod provision;
pub mod repo;
pub mod repo_env;
pub mod review_delivery;
pub mod runner;
pub mod runs;
pub(crate) mod runtime;
pub(crate) mod scratch;
pub mod server;
pub mod session;
pub(crate) mod session_layout;
pub mod session_manager;
pub(crate) mod setup;
pub mod shell;
pub(crate) mod slack;
pub(crate) mod tasks;
pub(crate) mod terminal;
pub mod user_token;
pub mod watch;
pub mod web;

/// Shared process state consumed by Loom's runtime services and REST adapter.
#[derive(Clone)]
pub struct AppState {
    pub db: db::Db,
    pub bus: weaver_core::events::EventBus,
    /// host:port the server is bound to, used to build child-process env.
    pub addr: String,
    /// Per-session embedded code-server lifecycle + reverse-proxy registry.
    pub ide: std::sync::Arc<ide::IdeManager>,
    /// The inbound GitHub trigger: its GitHub gateway (the `gh`-backed default)
    /// and per-repo rate limiter. Shared across requests; a test swaps in a fake
    /// gateway via [`github_trigger::GithubTrigger::with_gateway`].
    pub trigger: std::sync::Arc<github_trigger::GithubTrigger>,
    /// The registry of live ACP session tasks — the conversation routes drive
    /// sessions through it and subscribe to its SSE stream.
    pub acp: acp::AcpRegistry,
    /// Namespaced repository provisioning, capped-profile admission, and
    /// per-session Scratch mutation locks.
    pub launch_gate: launch_gate::RepoLaunchGate,
}

// Crate-local aliases keep Loom implementation imports short without exposing
// weaver-core's storage and domain modules as part of Loom's public API.
pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{branch, config, events, git};
