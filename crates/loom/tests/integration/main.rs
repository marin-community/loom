//! Integration suites for the loom server. Each module drives a real server
//! (shelling out to `git` and spawning `tapestry` terminal supervisors) through
//! the shared `fixtures::TestServer` harness. The cases are serialized
//! (`#[serial]`) because that harness mutates process-global env — see
//! `fixtures.rs`.
//!
//! The `hook` event → session-status path is covered separately by
//! `tests/hook_monitor.rs`, so it is not duplicated here.

mod fixtures;

mod archive;
mod auth;
mod branches;
mod conversation;
mod env;
mod ide;
mod overlookers;
mod pane;
mod recover;
mod repos;
mod scratch;
mod sessions;
mod shell;
mod terminal;
mod typed_client;
mod webhook;
