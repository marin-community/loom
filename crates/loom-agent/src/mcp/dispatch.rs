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
//! way the CLI does — a field the caller did not supply gets its `Default`
//! value, exactly what `Operands::from_matches` fills an absent flag with —
//! and then [`Operands::fill_context`] overwrites the context fields from the
//! session, same as the CLI. A field the schema marks `required` still has to
//! come from the caller; that is checked before the merge so a missing `id` is
//! a clear error rather than a silent `0`.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use weaver_api::operations::{
    artifacts, branches, channels, issues, permissions, sessions, ContextValues, Operands,
    Operation, OperationSpec, Render,
};
use weaver_api::Client;

use super::CapabilitySet;

type BoxFuture = Pin<Box<dyn Future<Output = Result<Value>> + Send>>;

/// One registered operation's MCP binding.
///
/// The closure is the only per-operation code, produced by [`bind`] from the
/// operation's own types — it cannot disagree with the descriptor beside it.
#[derive(Clone, Copy)]
pub(crate) struct McpBinding {
    pub(crate) operation: &'static OperationSpec,
    call: fn(&Client, &ContextValues, Value) -> BoxFuture,
}

/// Build a binding for one operation from its types alone.
///
/// Mirrors `crate::cli::dispatch::bind`: deserialize (completing what the
/// caller omitted), fill context once, invoke, render.
pub(crate) fn bind<O>() -> McpBinding
where
    O: Operation + Render,
    O::Input: Default,
    O::View: Default,
{
    McpBinding {
        operation: O::SPEC,
        call: |client, context, arguments| {
            // Cloned before the `async move` block so the returned future owns
            // its data rather than borrowing `client`/`context` — mirroring how
            // the CLI's `bind` uses `matches` synchronously before its own
            // `async move` (see `crate::cli::dispatch::bind`).
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
                let defaults = serde_json::to_value(O::Input::default()).map_err(|error| {
                    anyhow!("building defaults for {}: {error}", O::SPEC.id)
                })?;
                let merged = merge_defaults(arguments, defaults);
                let mut input: O::Input = serde_json::from_value(merged).map_err(|error| {
                    anyhow!("invalid arguments for {}: {error}", O::SPEC.id)
                })?;
                // Fetched once by the caller of `call` and handed down here —
                // not an extra `self_context()` round-trip per tool, as the old
                // per-adapter `project_input` functions each made their own.
                if !<O::Input as Operands>::CONTEXT.is_empty() {
                    input.fill_context(&context);
                }
                let value = serde_json::to_value(&input)?;
                let response = client.invoke_value(O::SPEC.id, value).await?;
                let output: O::Output = serde_json::from_value(response).map_err(|error| {
                    anyhow!("decoding response from {}: {error}", O::SPEC.id)
                })?;
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

/// Complete the caller's arguments with `Input::default()` for every field the
/// caller left out — context fields and declared-default fields alike. Values
/// the caller did supply always win.
fn merge_defaults(mut arguments: Value, defaults: Value) -> Value {
    let Value::Object(default_fields) = defaults else {
        return arguments;
    };
    let Some(given) = arguments.as_object_mut() else {
        return Value::Object(default_fields);
    };
    for (name, value) in default_fields {
        given.entry(name).or_insert(value);
    }
    arguments
}

/// Every registered operation's MCP binding.
///
/// One line per operation, exactly like `crate::cli::bindings()`. This list is
/// shared by every adapter's `tools/call` (see `call_tool` below), keyed by
/// operation id rather than by adapter — `loom_history`/`loom_messaging`
/// route some of their tools at operations bound here under a *different*
/// server name (`loom_session`), which is exactly why binding lives in one
/// shared table instead of one per adapter. A bundle that is not registered
/// here simply serves no MCP tool, so nothing else in this module needs to
/// change when the next bundle is ported.
pub(crate) fn bindings() -> Vec<McpBinding> {
    vec![
        bind::<issues::list::List>(),
        bind::<issues::get::Get>(),
        bind::<issues::create::Create>(),
        bind::<issues::backlog::create::Create>(),
        bind::<issues::close::Close>(),
        bind::<issues::reopen::Reopen>(),
        bind::<issues::delete::Delete>(),
        bind::<issues::tags::set::Set>(),
        bind::<issues::tags::delete::Delete>(),
        bind::<issues::actions::Actions>(),
        bind::<channels::list::List>(),
        bind::<channels::get::Get>(),
        bind::<channels::messages::list::List>(),
        bind::<channels::messages::create::Create>(),
        bind::<channels::create::Create>(),
        bind::<channels::subscription::set::Set>(),
        bind::<channels::read_marker::set::Set>(),
        bind::<channels::wait::Wait>(),
        bind::<artifacts::list::List>(),
        bind::<artifacts::get::Get>(),
        bind::<artifacts::write::Write>(),
        bind::<artifacts::delete::Delete>(),
        bind::<artifacts::history::History>(),
        bind::<artifacts::threads::list::List>(),
        bind::<artifacts::threads::comment::Comment>(),
        bind::<artifacts::threads::resolve::Resolve>(),
        bind::<sessions::context::Get>(),
        bind::<sessions::get::Get>(),
        bind::<sessions::summary::get::Get>(),
        bind::<sessions::status::get::Get>(),
        bind::<sessions::status::set::Set>(),
        bind::<sessions::history::list::List>(),
        bind::<sessions::history::search::Search>(),
        bind::<permissions::effective::get::Get>(),
        bind::<permissions::explain::Explain>(),
        bind::<permissions::requests::list::List>(),
        bind::<permissions::requests::create::Create>(),
        bind::<branches::slack::reply::Reply>(),
    ]
}

/// Resolve `server::tool`, fetch context if the operation needs any, and run
/// its binding. The single entry point a ported adapter's `tools/call` calls
/// into — everything above this line is generic over the operation's types,
/// and everything below it is registry lookups, not per-tool code.
/// Resolve the tool, *then* connect.
///
/// A call naming a tool that does not exist should say so, whatever else is
/// wrong with the environment. The hand-written adapters have always checked in
/// this order; the registry-driven ones lost it when they moved onto this
/// dispatcher and started building their client first — so `loom_context::not_a_tool`
/// reported a missing `LOOM_TOKEN` instead of an unknown tool.
pub(crate) async fn call_adapter_tool(
    adapter: &str,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    if !super::runtime_tool_allowed(tool) {
        bail!("{server} tool '{tool}' is not allowed by this session");
    }
    weaver_api::operation_for_mcp(server, tool)
        .with_context(|| format!("unknown {server} tool '{tool}'"))?;
    let client = super::runtime_client(adapter)?;
    call_tool(&client, server, tool, arguments).await
}

pub(crate) async fn call_tool(
    client: &Client,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    if !super::runtime_tool_allowed(tool) {
        bail!("{server} tool '{tool}' is not allowed by this session");
    }
    let operation = weaver_api::operation_for_mcp(server, tool)
        .with_context(|| format!("unknown {server} tool '{tool}'"))?;
    let binding = bindings()
        .into_iter()
        .find(|binding| binding.operation.id == operation.id)
        .with_context(|| format!("{server} tool '{tool}' has no registered MCP binding"))?;
    let context = if operation.context.is_empty() {
        ContextValues::default()
    } else {
        resolve_context(client).await?
    };
    (binding.call)(client, &context, arguments).await
}

async fn resolve_context(client: &Client) -> Result<ContextValues> {
    let context = client.self_context().await?;
    Ok(ContextValues {
        repo_root: context.repo_root,
        branch: context.branch_id,
        branch_name: context.branch_name,
        session: context.session_id,
    })
}

// -- Capability sets ----------------------------------------------------------

/// Group one MCP server's registered operations by the grants they name.
///
/// This is what replaces a hand-written `CapabilitySet` list: a set's tools
/// come from every operation whose `mcp.server` matches and whose `grants`
/// names it, so adding an operation to a grant widens the set automatically
/// rather than needing a matching edit here. Only the human-facing
/// description is still authored, via `describe` — the registry has no prose
/// field to derive it from.
pub(crate) fn derive_capability_sets(
    server: &'static str,
    group: &'static str,
    describe: fn(&str) -> &'static str,
) -> Vec<CapabilitySet> {
    let mut by_grant: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    for operation in weaver_api::operations() {
        let Some(projection) = operation.mcp.filter(|mcp| mcp.server == server) else {
            continue;
        };
        for grant in operation.grants {
            let tools = by_grant.entry(grant).or_default();
            if !tools.contains(&projection.tool) {
                tools.push(projection.tool);
            }
        }
    }
    by_grant
        .into_iter()
        .map(|(grant, mut tools)| {
            tools.sort_unstable();
            CapabilitySet {
                name: grant,
                group,
                version: grant.rsplit('@').next().unwrap_or("v1"),
                description: describe(grant),
                tools: Vec::leak(tools),
            }
        })
        .collect()
}

/// Whether `rule` (a Claude permission rule, `mcp__<server>__<tool>`) names one
/// of `server`'s registered MCP tools — derived from the registry rather than a
/// hand-maintained tool-name array.
pub(crate) fn is_permission_rule(server: &str, rule: &str) -> bool {
    rule.strip_prefix("mcp__")
        .and_then(|suffix| suffix.split_once("__"))
        .is_some_and(|(candidate, tool)| {
            candidate == server && weaver_api::operation_for_mcp(server, tool).is_some()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant that makes the binding table trustworthy: every
    /// operation that advertises an MCP tool has a binding here, mirroring
    /// `crate::cli::dispatch::tests::every_advertised_invocation_parses`.
    ///
    /// This covers every *registered* MCP projection, not just the adapters
    /// whose own server name matches their bundle: `loom_history` and
    /// `loom_messaging::status_update` reach operations bound here under a
    /// different server (`loom_session`), so binding is keyed by operation id
    /// rather than mirrored per adapter — see `call_tool` below.
    #[test]
    fn registered_operations_have_a_binding() {
        let bound: Vec<_> = bindings().iter().map(|binding| binding.operation.id).collect();
        let missing: Vec<_> = weaver_api::operations()
            .filter(|operation| operation.mcp.is_some())
            .map(|operation| operation.id)
            .filter(|id| !bound.contains(id))
            .collect();
        assert!(
            missing.is_empty(),
            "operations advertise an MCP tool but have no binding: {missing:?}"
        );
    }

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

    #[test]
    fn merge_defaults_keeps_caller_values() {
        let arguments = serde_json::json!({ "all": true });
        let defaults = serde_json::json!({ "all": false, "repo_root": "" });
        let merged = merge_defaults(arguments, defaults);
        assert_eq!(merged["all"], serde_json::json!(true));
        assert_eq!(merged["repo_root"], serde_json::json!(""));
    }

    #[test]
    fn issue_capability_sets_are_derived_from_the_registry() {
        let sets = derive_capability_sets("loom_issue", "issue", |_| "d");
        let read = sets.iter().find(|set| set.name == "loom/issues/read@v1").unwrap();
        let write = sets
            .iter()
            .find(|set| set.name == "loom/issues/write@v1")
            .unwrap();
        assert_eq!(read.tools, ["get", "list"]);
        assert_eq!(
            write.tools,
            [
                "actions",
                "add",
                "backlog_add",
                "close",
                "delete",
                "reopen",
                "tag_delete",
                "tag_set"
            ]
        );
    }
}
