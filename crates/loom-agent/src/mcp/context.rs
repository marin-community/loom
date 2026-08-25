//! Current caller context: the ids and links a session needs to address
//! itself.
//!
//! One tool, `sessions.context`, named in [`exports`].

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::sessions;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_context";

fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| vec![export::<sessions::context::Op>("get")])
}

// `sessions.context` claims `loom/sessions/read@v1`, the same grant
// `loom_session`'s read tools claim, but it is served by a different process
// and `expand_tool_set` resolves a set name to the first adapter that
// recognizes it. So this adapter names its own set, and only its tools are
// derived. `mcp/context/read@v1` is the name that set carried before the
// `loom/` rename.
const SET_NAME: &str = "loom/context/read@v1";
const RENAMED: &[(&str, &str)] = &[("mcp/context/read@v1", SET_NAME)];

pub(super) const ADAPTER: Adapter = Adapter {
    name: "context",
    server_name: SERVER_NAME,
    description: "Canonical identifiers and links for the calling session.",
    capability_sets,
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut sets = super::dispatch::capability_sets(exports(), "context", describe_capability);
        for set in &mut sets {
            set.name = SET_NAME;
        }
        sets.extend(super::dispatch::alias_capability_sets(&sets, RENAMED));
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

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
}

fn server_config() -> Value {
    super::builtin_server_config("context")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("context", SERVER_NAME, exports(), &name, arguments)
            .await
    })
}

fn serve_boxed() -> ServeFuture {
    Box::pin(super::serve_stdio(SERVER_NAME, tools, call_boxed))
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

    #[test]
    fn permission_rules_only_recognize_exported_tools() {
        assert!(is_permission_rule("mcp__loom_context__get"));
        assert!(!is_permission_rule("mcp__loom_context__bogus"));
    }
}
