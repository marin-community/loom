//! Text presentation for registered operations.
//!
//! One renderer per operation, shared by the CLI and MCP. Keeping it here rather
//! than in each adapter is why `loom issues list` and the `loom_issue::list`
//! tool cannot describe the same result differently.
//!
//! Renderers are pure functions of `Output` and the operation's view flags. An
//! operation whose text needs data the output does not carry should widen its
//! `Output` — not reach for a client mid-render, as the old CLI printers did.

pub mod artifacts;
pub mod channels;
pub mod issues;
pub mod mcps;
pub mod profiles;
pub mod reviews;
pub mod session_layout;
pub mod sessions;
pub mod settings;
pub mod watches;

/// Trim `text` to `max` characters, marking the cut with an ellipsis.
///
/// Counted in characters, not bytes: these are one-line summaries of text an
/// agent wrote, and slicing a multi-byte character in half would panic.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}
