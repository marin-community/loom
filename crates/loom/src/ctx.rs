//! The ambient facts a loom operation needs.
//!
//! Most of loom reads and writes rows, announces what changed, and occasionally
//! needs to tell a child process where to call back. That is all [`Ctx`] is —
//! and it is all the great majority of the codebase ever wanted from the
//! process-wide [`crate::AppState`], which additionally carries live registries
//! (editors, the GitHub trigger, ACP tasks, launch admission) that only the
//! orchestration and HTTP layers touch.
//!
//! Taking a `&Ctx` rather than a `&AppState` says so in the signature: this
//! function reaches storage and the bus, and nothing else. `AppState` derefs to
//! `Ctx`, so a caller holding one passes `&st` either way.

use weaver_core::db::Db;
use weaver_core::events::EventBus;

/// Durable state, the event bus, and this server's address.
#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
    pub bus: EventBus,
    /// host:port the server is bound to, used to build child-process env.
    pub addr: String,
}
