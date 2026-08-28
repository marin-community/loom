//! Repository work items.
//!
//! Every tool is a registered `issues.*` operation, named once in [`EXPORTS`]:
//! the catalogue, the capability sets, the permission rules, and the call path
//! are all read off that one list.

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::issues;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};

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
    description: "Repository work items, lifecycle, and free-form tags.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
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

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("issue", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("issue", exports(), &name, arguments).await
    })
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
}
