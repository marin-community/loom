//! Machine-local runtime helpers shared by Loom and Tapestry.

use std::{future::Future, path::PathBuf, pin::Pin};

/// Root directory for all weaver state on this machine.
pub fn weaver_home() -> PathBuf {
    if let Ok(p) = std::env::var("WEAVER_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".weaver")
}

/// A heap-allocated, type-erased future.
///
/// Returning this from a large async operation keeps its poll and drop code
/// in the defining crate, even when a caller awaits it from another crate.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Spawn a type-erased task so callers share one Tokio task harness.
pub fn spawn_boxed(fut: BoxFut<'static, ()>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(fut)
}
