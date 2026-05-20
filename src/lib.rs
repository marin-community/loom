//! weaver — a manager + launcher for concurrent agent workstreams.
//!
//! The unit of work is a [`workspace`]: one git worktree + one tmux session
//! running a coding agent, with a tracked goal and an evolving description.

pub mod agent;
pub mod client;
pub mod config;
pub mod db;
pub mod endpoint;
pub mod events;
pub mod git;
pub mod github;
pub mod monitor;
pub mod repo;
pub mod server;
pub mod summary;
pub mod tmux;
pub mod web;
pub mod workspace;

pub use db::Db;
pub use web::AppState;
