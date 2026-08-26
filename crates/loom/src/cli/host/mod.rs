//! Host-local commands: nothing here reaches the API.
//!
//! A command lands here when it manages this machine rather than the fleet —
//! it starts a process, prompts at a terminal, reads or writes a file under
//! `~/.weaver`, or opens loom's sqlite database directly. The operation
//! registry can never own one, which is why they sit apart from the commands
//! it can.

pub mod config;
pub mod contexts;
pub mod deployment;
pub mod federation;
pub mod server;
pub mod setup;
pub mod tokens;
