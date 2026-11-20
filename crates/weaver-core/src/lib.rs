//! weaver-core — pure model, db, git, events, config, and agent helpers
//! shared between the `weaver` CLI and the `loom` orchestrator. No HTTP, no
//! terminal management, no process spawning beyond `git`.

pub mod agent;
pub mod artifact;
pub mod branch;
pub mod config;
pub mod db;
pub mod discussion;
pub mod events;
pub mod git;
pub mod github;
pub mod issue;
pub mod migrations;
pub mod occurrence;
pub mod review;
pub mod schedule;
pub mod tags;
pub mod transcript;
pub mod watch;

pub use db::Db;
pub use weaver_runtime::{spawn_boxed, BoxFut};

pub mod process_identity;
