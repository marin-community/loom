//! Loom's server, public CLI, and transport adapters.
//!
//! Loom owns sessions (one terminal supervisor + one running agent per branch),
//! the REST API, the Vue web UI, the monitor loop, and recently-used repository
//! bookkeeping. `loom` is the single public command-line interface; its
//! host-facing and worktree-facing commands are clients of the same REST API.
//!
//! This crate is the HTTP/SSE/WebSocket adapter and the CLI. The engine beneath
//! it is split by persistent store, agent mechanism, policy, editor, forge,
//! launch, background-watch, and delivery subjects. Each layer is re-exported
//! here, so `loom::session`, `loom::AppState` and friends keep stable paths.

pub mod cli;
pub mod client;
pub mod endpoint;
pub mod server;
pub mod web;

pub use loom_agent::{acp, agent, custom_agents, mcp};
pub use loom_core::{launch, session_manager, shell};
pub use loom_ctx::Ctx;
pub use loom_ctx::{
    agent_kind, backend, changes, client_context, ctx, envfile, launch_gate, links, logs,
    loom_config, paths, runner, scratch,
};
pub use loom_deliver::{review_delivery, slack};
pub use loom_editor::{ide, terminal, EditorState};
pub use loom_forge::AppState;
pub use loom_forge::{
    github, github_app, github_manifest, github_trigger, lifecycle, repo, runtime, user_token,
};
pub use loom_launch::{handoff, metadata_assist, provision, setup};
pub use loom_policy::{auth, automation, custom_mcp, db, profile};
pub use loom_store::{
    agent_env, channels, chat, chatlog, github_access, history, permission_requests, profile_data,
    repo_env, review_inbox, runs, session, session_layout, slack_routes, status,
};
pub use loom_watch::{builtins, monitor, tasks, watch};

// Crate-local aliases keep Loom implementation imports short without exposing
// weaver-core's storage and domain modules as part of Loom's public API.
pub(crate) use weaver_core::db::Db;
pub(crate) use weaver_core::{config, events, git};
