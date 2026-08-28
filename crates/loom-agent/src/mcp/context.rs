//! Current caller context: the ids and links a session needs to address
//! itself.
//!
//! One tool, `sessions.context`, named in [`exports`].

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::sessions;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};

fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| vec![export::<sessions::context::Op>("get")])
}

// `sessions.context` claims `loom/sessions/read@v1`, the same grant as the
// session tools. This domain has its own capability identity so profiles can
// select only the caller-context surface.
const SET_NAME: &str = "loom/context/read@v1";

pub(super) const ADAPTER: Adapter = Adapter {
    name: "context",
    description: "Canonical identifiers and links for the calling session.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets = super::dispatch::capability_sets(exports(), "context", describe_capability);
        for set in &mut sets {
            set.name = SET_NAME;
        }
        sets
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/sessions/read@v1" => {
            "Resolve this session's canonical Loom resource identifiers and links."
        }
        _ => "Caller context operations.",
    }
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("context", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("context", exports(), &name, arguments).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_single_tool_is_the_context_operation() {
        let names: Vec<_> = exports().iter().map(|export| export.tool).collect();
        assert_eq!(names, ["get"]);
        assert_eq!(exports()[0].operation.id, "sessions.context");
    }
}
