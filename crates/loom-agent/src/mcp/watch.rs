//! Persistent scheduled-watch state, served by the generic registry dispatcher.

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::watches;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};

fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| vec![export::<watches::state::Op>("state")])
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "watch",
    description: "Persistent memory for the active scheduled watch.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| {
        super::dispatch::capability_sets(exports(), "watch", |_| {
            "Read or atomically replace the active scheduled watch's persistent memory."
        })
    })
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("watch", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("watch", exports(), &name, arguments).await
    })
}
