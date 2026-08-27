//! The generic MCP dispatcher, driven by the operation registry.
//!
//! This is the MCP twin of `crates/loom/src/cli/dispatch.rs`: there is no
//! per-tool code here beyond one `bind::<Op>()` line per registered operation in
//! [`bindings`]. Everything else — resolving a tool name to its operation,
//! filling session context, invoking, and rendering — is generic over the
//! operation's own types, so a newly registered bundle needs a new line in
//! [`bindings`] and nothing else.
//!
//! # Why arguments are merged with `Input::default()`
//!
//! An MCP caller only ever sends the fields [`Operands::schema`] advertises:
//! context fields are elided, and fields with a declared default are optional.
//! The server, however, deserializes the *whole* `Input` with no leniency of
//! its own (see `loom::web::operations::register`), so a partial JSON object
//! has to be completed before it is sent. This dispatcher completes it the same
//! way the CLI does: a field the caller did not supply gets its `Default`
//! value — exactly what `Operands::from_matches` fills an absent flag with —
//! then [`Operands::fill_context`] overwrites the context fields from the
//! session. A field the schema marks `required` still has to come from the
//! caller; missing required fields are checked before the merge and fail with
//! a clear error.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use weaver_api::operations::{ContextValues, Operands, Operation, OperationSpec, Render};
use weaver_api::Client;

use super::CapabilitySet;

type BoxFuture = Pin<Box<dyn Future<Output = Result<Value>> + Send>>;

/// One tool an adapter exports: the name it advertises and the operation that
/// name means.
///
/// This is the only place the two are tied together. The catalogue an adapter
/// advertises, the capability sets it offers, the permission rules it
/// recognizes, and the code that runs a call are all read off the same list, so
/// a tool cannot be advertised without being served or served without being
/// advertised.
#[derive(Clone, Copy)]
pub(crate) struct Export {
    pub(crate) tool: &'static str,
    pub(crate) operation: &'static OperationSpec,
    call: fn(&Client, &ContextValues, Value) -> BoxFuture,
}

/// What an adapter may export.
///
/// An agent's tool has to be an operation an agent may call, and the
/// schema-driven dispatcher below can only serve a JSON one. Both are asserted
/// where the export list is written, so exporting a human-only operation —
/// `sessions.permissions.answer`, say — or a streaming one is a compile error.
trait Exportable: Operation {
    const CHECKED: () = {
        assert!(
            Self::SPEC.actor.agent_reachable(),
            "an MCP tool must name an agent-reachable operation",
        );
        assert!(
            Self::SPEC.io.is_json(),
            "an MCP tool must name a JSON operation",
        );
    };
}

impl<O: Operation> Exportable for O {}

/// Export one operation under `tool`, serving it from its own types.
///
/// Mirrors `crate::cli::dispatch::bind`: deserialize (completing what the
/// caller omitted), fill context once, invoke, render.
pub(crate) fn export<O>(tool: &'static str) -> Export
where
    O: Operation + Render,
    O::Input: Default,
    O::View: Default,
{
    let () = <O as Exportable>::CHECKED;
    Export {
        tool,
        operation: O::SPEC,
        call: |client, context, arguments| {
            // Cloned so the returned future owns its data rather than borrowing
            // `client`/`context` — mirrors `crate::cli::dispatch::bind`, which
            // clones `matches` the same way before its own `async move`.
            let client = client.clone();
            let context = context.clone();
            Box::pin(async move {
                let schema = <O::Input as Operands>::schema();
                let missing = missing_required(&schema, &arguments);
                if !missing.is_empty() {
                    bail!(
                        "{} missing required argument(s): {}",
                        O::SPEC.id,
                        missing.join(", ")
                    );
                }
                let mut input: O::Input = serde_json::from_value(arguments)
                    .map_err(|error| anyhow!("invalid arguments for {}: {error}", O::SPEC.id))?;
                // Context is fetched once by the caller and handed down to all
                // operations in a single request.
                if !<O::Input as Operands>::CONTEXT.is_empty() {
                    input.fill_context(&context);
                }
                let value = serde_json::to_value(&input)?;
                let response = client.invoke_value(O::SPEC.id, value).await?;
                let output: O::Output = serde_json::from_value(response)
                    .map_err(|error| anyhow!("decoding response from {}: {error}", O::SPEC.id))?;
                let text = O::text(&output, &O::View::default());
                super::structured_result(&text, &output)
            })
        },
    }
}

/// Names the schema marks `required` that the caller did not supply. Context
/// fields never appear here — [`Operands::schema`] has already elided them.
fn missing_required(schema: &Value, arguments: &Value) -> Vec<String> {
    let Some(required) = schema.get("required").and_then(Value::as_array) else {
        return Vec::new();
    };
    let object = arguments.as_object();
    required
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| object.is_none_or(|object| !object.contains_key(*name)))
        .map(str::to_string)
        .collect()
}

/// Resolve `tool` against this adapter's exports, fetch context if the
/// operation needs any, and run it.
///
/// Resolves the tool before connecting: an unknown tool must be reported as
/// such, not masked by an unrelated client failure like a missing `LOOM_TOKEN`.
pub(crate) async fn call_adapter_tool(
    adapter: &str,
    server: &str,
    exports: &[Export],
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    if !super::runtime_tool_allowed(tool) {
        bail!("{server} tool '{tool}' is not allowed by this session");
    }
    lookup(exports, tool).with_context(|| format!("unknown {server} tool '{tool}'"))?;
    let client = super::runtime_client(adapter)?;
    call_tool(&client, server, exports, tool, arguments).await
}

pub(crate) async fn call_tool(
    client: &Client,
    server: &str,
    exports: &[Export],
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    if !super::runtime_tool_allowed(tool) {
        bail!("{server} tool '{tool}' is not allowed by this session");
    }
    let export =
        lookup(exports, tool).with_context(|| format!("unknown {server} tool '{tool}'"))?;
    let context = if export.operation.context.is_empty() {
        ContextValues::default()
    } else {
        resolve_context(client).await?
    };
    (export.call)(client, &context, arguments).await
}

async fn resolve_context(client: &Client) -> Result<ContextValues> {
    use weaver_api::operations::sessions::context;
    let context = client
        .invoke::<context::Op>(&context::Input {
            session: String::new(),
        })
        .await?;
    Ok(ContextValues {
        repo_root: context.repo_root,
        branch: context.branch_id,
        branch_name: context.branch_name,
        session: context.session_id,
    })
}

/// One adapter's advertised catalogue, in the order it declared.
///
/// Order is observable to MCP clients, so it is the export list's order rather
/// than something sorted here.
pub(crate) fn tools(exports: &[Export]) -> Value {
    Value::Array(
        exports
            .iter()
            .map(|export| {
                json!({
                    "name": export.tool,
                    "description": export.operation.summary,
                    "inputSchema": (export.operation.schema)(),
                })
            })
            .collect(),
    )
}

pub(crate) fn lookup<'a>(exports: &'a [Export], tool: &str) -> Option<&'a Export> {
    exports.iter().find(|export| export.tool == tool)
}

/// Whether `rule` (a Claude permission rule, `mcp__<server>__<tool>`) names one
/// of this adapter's tools.
pub(crate) fn is_permission_rule(server: &str, exports: &[Export], rule: &str) -> bool {
    rule.strip_prefix("mcp__")
        .and_then(|suffix| suffix.split_once("__"))
        .is_some_and(|(candidate, tool)| candidate == server && lookup(exports, tool).is_some())
}

/// The permission rules one capability set expands to.
pub(crate) fn expand_tool_set(
    server: &str,
    sets: &[CapabilitySet],
    name: &str,
) -> Option<Vec<String>> {
    sets.iter().find(|set| set.name == name).map(|set| {
        set.tools
            .iter()
            .map(|tool| format!("mcp__{server}__{tool}"))
            .collect()
    })
}

/// Group an adapter's exports into capability sets by the grants they name.
///
/// A set's tools are every export whose operation names that grant, so adding
/// an operation to a grant widens the set with no second edit. Descriptions are
/// supplied by the adapter because the registry carries no prose for a grant.
pub(crate) fn capability_sets(
    exports: &[Export],
    group: &'static str,
    describe: fn(&str) -> &'static str,
) -> Vec<CapabilitySet> {
    let mut by_grant: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    for export in exports {
        for grant in export.operation.grants {
            let tools = by_grant.entry(grant).or_default();
            if !tools.contains(&export.tool) {
                tools.push(export.tool);
            }
        }
    }
    by_grant
        .into_iter()
        .map(|(grant, tools)| CapabilitySet {
            name: grant,
            group,
            version: grant.rsplit('@').next().unwrap_or("v1"),
            description: describe(grant),
            tools: Vec::leak(tools),
        })
        .collect()
}

/// Re-publish derived sets under names they were renamed away from.
///
/// A set that was renamed is still the same set: a session pinned to
/// `mcp/artifact/read@v1` must resolve exactly what `loom/artifacts/read@v1`
/// resolves, including operations exported since the rename. Restating the tool
/// list under the old name would freeze it at the membership it had on the day
/// the alias was written.
pub(crate) fn alias_capability_sets(
    sets: &[CapabilitySet],
    superseded: &[(&'static str, &'static str)],
) -> Vec<CapabilitySet> {
    superseded
        .iter()
        .map(|(before, after)| {
            let set = sets
                .iter()
                .find(|set| set.name == *after)
                .unwrap_or_else(|| panic!("`{before}` names `{after}`, which nothing exports"));
            CapabilitySet {
                name: before,
                group: set.group,
                version: set.version,
                description: set.description,
                tools: set.tools,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_required_ignores_defaulted_and_context_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" }, "note": { "type": "string" } },
            "required": ["id"]
        });
        assert_eq!(
            missing_required(&schema, &serde_json::json!({})),
            vec!["id".to_string()]
        );
        assert!(missing_required(&schema, &serde_json::json!({ "id": 7 })).is_empty());
    }
}
