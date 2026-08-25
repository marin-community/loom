//! Repository work items.
//!
//! Every tool is a registered `issues.*` operation, named once in [`EXPORTS`]:
//! the catalogue, the capability sets, the permission rules, and the call path
//! are all read off that one list.

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::issues;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_issue";

/// The tools this server exports, in the order it advertises them.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<issues::actions::Op>("actions"),
            export::<issues::create::Op>("add"),
            export::<issues::backlog::create::Op>("backlog_add"),
            export::<issues::close::Op>("close"),
            export::<issues::delete::Op>("delete"),
            export::<issues::get::Op>("get"),
            export::<issues::list::Op>("list"),
            export::<issues::reopen::Op>("reopen"),
            export::<issues::tags::delete::Op>("tag_delete"),
            export::<issues::tags::set::Op>("tag_set"),
        ]
    })
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "issue",
    server_name: SERVER_NAME,
    description: "Repository work items, lifecycle, and free-form tags.",
    capability_sets,
    exports,
    superseded: &[
        ("mcp/issue/read@v1", "loom/issues/read@v1"),
        ("mcp/issue/write@v1", "loom/issues/write@v1"),
    ],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| super::dispatch::capability_sets(exports(), "issue", describe_capability))
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/issues/read@v1" => "List and inspect work items in this session's repository.",
        "loom/issues/write@v1" => "Create, update, tag, and remove repository work items.",
        _ => "Repository work item operations.",
    }
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
}

fn server_config() -> Value {
    super::builtin_server_config("issue")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("issue", SERVER_NAME, exports(), &name, arguments).await
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_sets_are_grouped_by_grant() {
        assert_eq!(expand_tool_set("loom/issues/read@v1").unwrap().len(), 2);
        assert_eq!(expand_tool_set("loom/issues/write@v1").unwrap().len(), 8);
        assert!(expand_tool_set("loom/issues/nonexistent@v1").is_none());
    }

    #[test]
    fn permission_rules_only_recognize_exported_tools() {
        assert!(is_permission_rule("mcp__loom_issue__list"));
        assert!(!is_permission_rule("mcp__loom_issue__bogus"));
        assert!(!is_permission_rule("mcp__other_server__list"));
    }
}
