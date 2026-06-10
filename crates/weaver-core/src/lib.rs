//! weaver-core — pure model, db, git, events, config, and agent helpers
//! shared between the `weaver` CLI and the `loom` orchestrator. No HTTP, no
//! tmux, no process spawning beyond `git`.

pub mod agent;
pub mod branch;
pub mod config;
pub mod db;
pub mod events;
pub mod git;
pub mod github;
pub mod issue;
pub mod migrations;
pub mod overlooker;
pub mod plan;
pub mod repo_config;
pub mod tags;

pub use db::Db;
