//! Text rendering for session-dashboard layout operations.
//!
//! Every layout operation answers with the whole layout at its new revision, so
//! there is one formatter and thirteen bindings to it.

use crate::dto::SessionLayoutView;
use crate::operations::session_layout::{defaults, get, groups, r#move, reorder, restore, spaces};
use crate::operations::{NoView, Render};

/// Spaces, their groups, the sessions placed in each, and the placement
/// defaults — indented to show the nesting, prefixed by the revision a
/// subsequent `--revision` would name.
fn tree(layout: &SessionLayoutView) -> String {
    let mut lines = vec![format!("session layout revision {}", layout.revision)];
    for space in &layout.spaces {
        lines.push(format!(
            "{}  {}  (rank {})",
            space.id, space.name, space.rank
        ));
        for group in &space.groups {
            let disclosure = if group.collapsed {
                "collapsed"
            } else {
                "expanded"
            };
            lines.push(format!(
                "  {}  {}  (rank {}, {disclosure})",
                group.id, group.name, group.rank
            ));
            for session_id in &group.session_ids {
                lines.push(format!("    {session_id}"));
            }
        }
    }
    if !layout.defaults.is_empty() {
        lines.push("defaults:".to_string());
        for default in &layout.defaults {
            lines.push(format!(
                "  {}:{} -> {}",
                default.selector_kind, default.selector_value, default.group_id
            ));
        }
    }
    lines.join("\n")
}

macro_rules! prints_the_layout {
    ($($op:ty),+ $(,)?) => {
        $(
            impl Render for $op {
                fn text(output: &SessionLayoutView, _: &NoView) -> String {
                    tree(output)
                }
            }
        )+
    };
}

prints_the_layout!(
    get::Op,
    spaces::create::Op,
    spaces::update::Op,
    spaces::delete::Op,
    groups::create::Op,
    groups::update::Op,
    groups::delete::Op,
    groups::preference::set::Op,
    r#move::Op,
    reorder::Op,
    restore::Op,
    defaults::set::Op,
    defaults::delete::Op,
);
