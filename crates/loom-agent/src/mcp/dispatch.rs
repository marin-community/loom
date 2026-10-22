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
use weaver_api::operations::{
    ContextValues, Operands, Operation, OperationScope, OperationSpec, Render,
};
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

/// What a person's tool may export: the [`Exportable`] rule with people in
/// place of agents. `SessionOnly` would hand a person a session's own
/// credential, so exporting it — or an `Admin`, `Internal` or `Anonymous`
/// operation — is a compile error.
trait UserExportable: Operation {
    const CHECKED: () = {
        assert!(
            Self::SPEC.actor.user_reachable(),
            "a person's MCP tool must name a SessionSelf or User operation",
        );
        assert!(
            Self::SPEC.io.is_json(),
            "an MCP tool must name a JSON operation",
        );
    };
}

impl<O: Operation> UserExportable for O {}

/// Check the schema's required names, then deserialize the caller's partial
/// object; serde fills every declared default, and context fields stay empty.
fn decode_input<O: Operation>(schema: &Value, arguments: Value) -> Result<O::Input> {
    let missing = missing_required(schema, &arguments);
    if !missing.is_empty() {
        bail!(
            "{} missing required argument(s): {}",
            O::SPEC.id,
            missing.join(", ")
        );
    }
    serde_json::from_value(arguments)
        .map_err(|error| anyhow!("invalid arguments for {}: {error}", O::SPEC.id))
}

/// Decode an operation's JSON response and render it as an MCP tool result.
fn render_output<O>(response: Value) -> Result<Value>
where
    O: Operation + Render,
    O::View: Default,
{
    let output: O::Output = serde_json::from_value(response)
        .map_err(|error| anyhow!("decoding response from {}: {error}", O::SPEC.id))?;
    let text = O::text(&output, &O::View::default());
    super::structured_result(&text, &output)
}

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
                let mut input = decode_input::<O>(&schema, arguments)?;
                // Context is fetched once by the caller and handed down to all
                // operations in a single request.
                if !<O::Input as Operands>::CONTEXT.is_empty() {
                    input.fill_context(&context);
                }
                let value = serde_json::to_value(&input)?;
                let response = client.invoke_value(O::SPEC.id, value).await?;
                render_output::<O>(response)
            })
        },
    }
}

/// One tool Loom's remote MCP endpoint offers a person.
///
/// The person's own credential is the authority and there is no session to
/// draw context from, so the transport invokes the operation directly rather
/// than through a [`Client`]: [`UserExport::prepare`] turns arguments into the
/// operation's input, the transport runs it through the same registered
/// dispatcher and `authorize()` as REST, and [`UserExport::respond`] renders
/// the result. Context operands stay empty — `sessions.launch` gets no parent
/// branch and counts as a person's launch — except the one naming the
/// resource the operation acts on, which a person must supply.
#[derive(Clone, Copy)]
pub struct UserExport {
    pub tool: &'static str,
    pub operation: &'static OperationSpec,
    description: Option<&'static str>,
    /// The only arguments the tool accepts, when narrower than the operation.
    fields: Option<&'static [&'static str]>,
    /// Arguments that default to a setting's value when the caller omits them.
    setting_defaults: &'static [(&'static str, &'static str)],
    decode: fn(&Value, Value) -> Result<Value>,
    render: fn(Value) -> Result<Value>,
}

/// Export one operation as a person's tool, serving it from its own types.
pub(crate) fn export_user<O>(tool: &'static str) -> UserExport
where
    O: Operation + Render,
    O::View: Default,
{
    let () = <O as UserExportable>::CHECKED;
    UserExport {
        tool,
        operation: O::SPEC,
        description: None,
        fields: None,
        setting_defaults: &[],
        decode: |schema, arguments| {
            let input = decode_input::<O>(schema, arguments)?;
            Ok(serde_json::to_value(&input)?)
        },
        render: render_output::<O>,
    }
}

impl UserExport {
    /// Replace the operation's summary, written for agents, with one written
    /// for an outside assistant acting for a person.
    pub(crate) const fn describe(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }

    /// Accept only these arguments. The resource field is always accepted.
    pub(crate) const fn only(mut self, fields: &'static [&'static str]) -> Self {
        self.fields = Some(fields);
        self
    }

    /// Fill `field` from the setting `key` when the caller omits it.
    pub(crate) const fn default_from_settings(
        mut self,
        defaults: &'static [(&'static str, &'static str)],
    ) -> Self {
        self.setting_defaults = defaults;
        self
    }

    /// The `(argument, setting key)` pairs the transport resolves before
    /// [`Self::prepare`]; this crate has no business reading settings.
    pub fn setting_defaults(&self) -> &'static [(&'static str, &'static str)] {
        self.setting_defaults
    }

    pub fn description(&self) -> &'static str {
        self.description.unwrap_or(self.operation.summary)
    }

    /// The argument schema a person's client sees: the operation's caller
    /// schema, with the resource field required, narrowed to [`Self::only`],
    /// and closed to anything else.
    pub fn input_schema(&self) -> Value {
        let mut schema = (self.operation.schema)();
        let Some(object) = schema.as_object_mut() else {
            return schema;
        };
        let properties = object
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("an operation schema's properties are an object");
        let resource = resource_field(self.operation.scope);
        if let Some((name, description)) = resource {
            properties.insert(
                name.to_string(),
                json!({ "type": "string", "minLength": 1, "description": description }),
            );
        }
        if let Some(fields) = self.fields {
            properties.retain(|name, _| {
                fields.contains(&name.as_str()) || resource.is_some_and(|(r, _)| r == name)
            });
        }
        let names: Vec<String> = properties.keys().cloned().collect();
        let mut required: Vec<Value> = object
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|name| {
                name.as_str()
                    .is_some_and(|name| names.iter().any(|n| n == name))
            })
            .cloned()
            .collect();
        if let Some((name, _)) = resource {
            if !required.iter().any(|value| value == name) {
                required.push(json!(name));
            }
        }
        if required.is_empty() {
            object.remove("required");
        } else {
            object.insert("required".to_string(), Value::Array(required));
        }
        object.insert("additionalProperties".to_string(), json!(false));
        schema
    }

    /// Turn a person's arguments into the operation's complete JSON input.
    ///
    /// Fails closed on any argument the schema does not advertise, so nothing
    /// outside the reviewed surface — a parent branch, a host path, a launch
    /// selection — can be smuggled through.
    pub fn prepare(&self, arguments: Value) -> Result<Value> {
        let arguments = match arguments {
            Value::Null => json!({}),
            Value::Object(_) => arguments,
            _ => bail!("{} arguments must be an object", self.tool),
        };
        let schema = self.input_schema();
        let known = schema.get("properties").and_then(Value::as_object);
        let unknown: Vec<&str> = arguments
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys())
            .filter(|name| known.is_none_or(|known| !known.contains_key(*name)))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            bail!(
                "{} does not accept argument(s): {}",
                self.tool,
                unknown.join(", ")
            );
        }
        (self.decode)(&schema, arguments)
    }

    /// Render the operation's JSON response as an MCP tool result.
    pub fn respond(&self, response: Value) -> Result<Value> {
        (self.render)(response)
    }
}

/// The field a person names the resource with, by the operation's scope, and
/// how to describe it to them. Mirrors the `Scoped` impl the operation macro
/// derives from `scope`.
fn resource_field(scope: OperationScope) -> Option<(&'static str, &'static str)> {
    match scope {
        OperationScope::Session => {
            Some(("session", "Session id, as returned by `launch` or `list`."))
        }
        OperationScope::Channel => Some((
            "channel",
            "Channel id. A session's own channel id is its session id.",
        )),
        OperationScope::Branch => Some(("branch", "Branch id or name.")),
        OperationScope::Repository => Some(("repo_root", "Repository root.")),
        OperationScope::Global => None,
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
    exports: &[Export],
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    if !super::runtime_adapter_tool_allowed(adapter, tool) {
        bail!("{adapter} tool '{tool}' is not allowed by this session");
    }
    lookup(exports, tool).with_context(|| format!("unknown {adapter} tool '{tool}'"))?;
    let client = super::runtime_client(adapter)?;
    call_tool(&client, adapter, exports, tool, arguments).await
}

pub(crate) async fn call_tool(
    client: &Client,
    domain: &str,
    exports: &[Export],
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    let export =
        lookup(exports, tool).with_context(|| format!("unknown {domain} tool '{tool}'"))?;
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

/// The permission rules one capability set expands to.
pub(crate) fn expand_tool_set(
    domain: &str,
    sets: &[CapabilitySet],
    name: &str,
) -> Option<Vec<String>> {
    sets.iter().find(|set| set.name == name).map(|set| {
        set.tools
            .iter()
            .map(|tool| super::builtin_permission_rule(domain, tool))
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
