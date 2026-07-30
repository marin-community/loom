//! Everything that reaches outside this process.
//!
//! Git and GitHub, Slack, container and editor lifecycles, the watch scheduler,
//! session provisioning and teardown. This is also where [`AppState`] lives: the
//! process-wide registries — editors, the inbound GitHub trigger, live ACP
//! tasks, launch admission — belong to the layer that drives them, not to the
//! domain underneath.

pub mod builtins;
pub mod github;
pub mod github_app;
pub mod github_manifest;
pub mod github_trigger;
pub mod handoff;
pub mod ide;
pub mod launch;
pub mod lifecycle;
pub mod metadata_assist;
pub mod monitor;
pub mod provision;
pub mod repo;
pub mod review_delivery;
pub mod runtime;
pub mod setup;
pub mod slack;
pub mod tasks;
pub mod terminal;
pub mod user_token;
pub mod watch;

// The layers below, re-exported so `crate::session` and friends resolve here.
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

/// Shared process state consumed by Loom's runtime services and REST adapter.
///
/// The [`Ctx`] inside is what most of loom actually needs; the fields beside it
/// are live, process-local registries that only this layer and the HTTP adapter
/// above it touch. `AppState` derefs to its `Ctx`, so `st.db` and `st.bus` read
/// the same whichever one a function was handed.
#[derive(Clone)]
pub struct AppState {
    pub ctx: Ctx,
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

impl AppState {
    /// The slice of process state an ACP session task needs — durable state
    /// plus the task registry, without the editor/GitHub/admission registries
    /// it never reads.
    pub fn acp_ctx(&self) -> acp::AcpCtx {
        acp::AcpCtx {
            ctx: self.ctx.clone(),
            acp: self.acp.clone(),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = Ctx;
    fn deref(&self) -> &Ctx {
        &self.ctx
    }
}

// Crate-local aliases keep implementation imports short without exposing
// weaver-core's storage and domain modules as part of the public API.
pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{branch, config, events, git};
