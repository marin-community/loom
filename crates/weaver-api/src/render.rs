//! Text presentation for registered operations.
//!
//! One renderer per operation, shared by the CLI and MCP. Keeping it here rather
//! than in each adapter is why `loom issues list` and the `loom_issue::list`
//! tool cannot describe the same result differently.
//!
//! Renderers are pure functions of `Output` and the operation's view flags. An
//! operation whose text needs data the output does not carry should widen its
//! `Output` — not reach for a client mid-render, as the old CLI printers did.

pub mod channels;
pub mod issues;
pub mod reviews;
pub mod session_layout;
