//! Text rendering for the trusted MCP registry.

use crate::dto::McpRegistryView;
use crate::operations::mcps;
use crate::operations::{NoView, Render};

impl Render for mcps::get::Op {
    /// The builtin capability sets, then the custom servers. One table, because
    /// what an operator is choosing between is a capability, and where it came
    /// from is a column: a builtin carries a version, a custom server a
    /// revision and the validation state that decides whether it may run.
    fn text(output: &McpRegistryView, _: &NoView) -> String {
        let mut lines = Vec::new();
        for set in &output.capability_sets {
            lines.push(format!(
                "{:<30} {:<4} {:<12} {}",
                set.name, set.version, set.adapter, set.description
            ));
        }
        for server in &output.custom_servers {
            lines.push(format!(
                "{:<30} r{:<3} {:<12} {}",
                server.identity, server.revision, server.validation_state, server.description
            ));
        }
        if lines.is_empty() {
            return "(no MCP capability sets)".to_string();
        }
        lines.join("\n")
    }
}
