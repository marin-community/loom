//! loom — the optional orchestrator binary that sits on top of `weaver-core`.
//!
//! Loom owns sessions (one terminal supervisor + one running agent per branch),
//! the REST API, the Vue web UI, the monitor loop, and recently-used repository
//! bookkeeping. The agent-facing `weaver` CLI does not depend on loom; running
//! loom is purely additive.
//!
//! This crate is the HTTP/SSE/WebSocket adapter and the CLI. The engine beneath
//! it is three crates, stacked: [`loom_ctx`] (leaf utilities and the ambient
//! `Ctx`), [`loom_domain`] (persistent state and the rules over it), and
//! [`loom_ops`] (everything that reaches outside the process). Each is
//! re-exported here, so `loom::session`, `loom::AppState` and friends name the
//! same items they always did regardless of which crate now defines them.

pub mod client;
pub mod endpoint;
pub mod server;
pub mod web;

pub use loom_ctx::Ctx;
pub use loom_ctx::{
    backend, changes, client_context, ctx, envfile, launch_gate, links, logs, loom_config, runner,
    scratch,
};
pub use loom_domain::{
    acp, agent, agent_env, auth, automation, channels, chat, chatlog, custom_agents, custom_mcp,
    db, history, mcp, profile, repo_env, review_inbox, runs, session, session_layout,
    session_manager, shell, status,
};
pub use loom_ops::AppState;
pub use loom_ops::{
    builtins, github, github_app, github_manifest, github_trigger, handoff, ide, launch, lifecycle,
    metadata_assist, monitor, provision, repo, review_delivery, runtime, setup, slack, tasks,
    terminal, user_token, watch,
};

// Crate-local aliases keep Loom implementation imports short without exposing
// weaver-core's storage and domain modules as part of Loom's public API.
pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{config, events, git};
