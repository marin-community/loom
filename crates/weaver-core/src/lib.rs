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
pub mod repo_config;
pub mod review;
pub mod tags;
pub mod transcript;
pub mod watch;

pub use db::Db;

/// A heap-allocated, type-erased future.
///
/// An `async fn`'s state machine is codegen'd wherever it is awaited, so a
/// large one awaited from another crate lands in *that* crate's compile unit,
/// and transitively in whichever crate finally polls the chain. Returning this
/// instead pins the state machine to the crate that defines it: callers see a
/// vtable, not a generic instantiation. Worth it only for the big ones — the
/// eight largest in this workspace were 6% of `loom`'s LLVM IR and 18% of its
/// rebuild time.
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Spawn a type-erased task.
///
/// `tokio::spawn` is generic over the future, so every distinct future spawned
/// stamps out its own copy of the task harness — poll, drop, join handle, and
/// the panic-catch wrapper, ~680 lines of LLVM IR per call site. Erasing the
/// type first collapses all of them onto one instantiation.
pub fn spawn_boxed(fut: BoxFut<'static, ()>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(fut)
}
