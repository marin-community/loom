//! Leaf utilities and the ambient context every layer above reads.
//!
//! Nothing here knows what a session is. These are the pieces with no loom
//! dependency of their own: the terminal backend, the diff reader, the launch
//! admission gate, dashboard link formatting, the log tail, the one-shot runner,
//! the scratch store, `.env` and `loom.toml` parsing — and [`Ctx`], the storage
//! handle plus event bus that every layer above threads through.

pub mod backend;
pub mod changes;
pub mod client_context;
pub mod ctx;
pub mod envfile;
pub mod launch_gate;
pub mod links;
pub mod logs;
pub mod loom_config;
pub mod runner;
pub mod scratch;

pub use ctx::Ctx;
