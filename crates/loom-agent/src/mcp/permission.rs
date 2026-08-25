//! Effective access and approval requests, served by the generic registry
//! dispatcher.
//!
//! Every tool here is a registered `permissions.*` operation: `tools/list` is
//! read off [`exports`], and `tools/call` is `super::dispatch::call_tool`,
//! which renders each operation's own JSON as
//! the response — no per-tool summary formatting lives here.
//! `permissions.requests.approve`/`.deny` stay unreachable through this
//! adapter because they are `actor = User`, not `SessionSelf`: the registry
//! rejects an MCP projection on a non-agent-reachable operation, so this
//! adapter needs no extra check to keep an agent from approving its own
//! permission request.

use std::sync::OnceLock;

use serde_json::Value;

use weaver_api::operations::permissions;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ServeFuture, ToolFuture};

const SERVER_NAME: &str = "loom_permission";

/// The tools this server exports, in the order it advertises them.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<permissions::effective::get::Op>("show"),
            export::<permissions::explain::Op>("explain"),
            export::<permissions::requests::list::Op>("requests"),
            export::<permissions::requests::create::Op>("request"),
        ]
    })
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "permission",
    server_name: SERVER_NAME,
    description: "Effective operation grants and durable human approval requests.",
    capability_sets,
    exports,
    superseded: &[],
    expand_tool_set,
    is_permission_rule,
    server_config,
    tools,
    serve: serve_boxed,
};

/// Capability sets are derived from the registry: every `permissions.*` operation
/// whose MCP projection targets this server contributes its tool to the set named
/// by its grant.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        super::dispatch::capability_sets(exports(), "permissions", describe_capability)
    })
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/permissions/read@v1" => {
            "Inspect effective Loom operations, repository scope, and access requests."
        }
        "loom/permissions/request@v1" => {
            "Request a human-approved expansion of external session access."
        }
        _ => "Effective operation grants and durable human approval requests.",
    }
}

fn is_permission_rule(rule: &str) -> bool {
    super::dispatch::is_permission_rule(SERVER_NAME, exports(), rule)
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set(SERVER_NAME, capability_sets(), name)
}

fn server_config() -> Value {
    super::builtin_server_config("permission")
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("permission", SERVER_NAME, exports(), &name, arguments)
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
    fn permission_rules_only_recognize_registered_tools() {
        assert!(is_permission_rule("mcp__loom_permission__show"));
        assert!(!is_permission_rule("mcp__loom_permission__bogus"));
        assert!(!is_permission_rule("mcp__other_server__show"));
    }
}
