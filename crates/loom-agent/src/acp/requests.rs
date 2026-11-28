//! Loom's outgoing ACP wire types over the agent-client-protocol crate.
//!
//! The crate owns the connection engine — transport, request/response
//! correlation, ordered dispatch, and the method-not-found default for
//! requests loom does not handle. These types keep the *payloads* raw: ACP's
//! `configOptions` and the harness extensions around it are not strictly
//! typed across harnesses (Claude, Codex, Opencode and Cursor each spell
//! effort and fast differently), so a typed response would silently drop
//! what a harness actually advertised. `initialize` is the exception: its
//! response is stable, so the crate's own typed request is used.

use agent_client_protocol::{JsonRpcNotification, JsonRpcRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `session/new` — a fresh session; response kept raw for
/// [`crate::catalog::selectors`] to read.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "session/new", response = serde_json::Value)]
pub struct NewSessionRequest {
    pub cwd: String,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<Value>,
    #[serde(
        rename = "additionalDirectories",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub additional_directories: Vec<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// `session/new` with the same shape the hand-rolled wire builder produced.
pub fn new_session(
    cwd: &str,
    mcp_servers: &[Value],
    additional_directories: &[String],
    meta: Option<&Value>,
) -> NewSessionRequest {
    NewSessionRequest {
        cwd: cwd.to_string(),
        mcp_servers: mcp_servers.to_vec(),
        additional_directories: additional_directories.to_vec(),
        meta: meta.cloned(),
    }
}

/// `session/load` — resume an existing agent session id; history replays as
/// `session/update` notifications before the response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "session/load", response = serde_json::Value)]
pub struct LoadSessionRequest {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub cwd: String,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<Value>,
    #[serde(
        rename = "additionalDirectories",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub additional_directories: Vec<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// `session/load` with the same shape the hand-rolled wire builder produced.
pub fn load_session(
    session_id: &str,
    cwd: &str,
    mcp_servers: &[Value],
    additional_directories: &[String],
    meta: Option<&Value>,
) -> LoadSessionRequest {
    LoadSessionRequest {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        mcp_servers: mcp_servers.to_vec(),
        additional_directories: additional_directories.to_vec(),
        meta: meta.cloned(),
    }
}

/// The prompt blocks for [`PromptRequest`] and [`SteeringRequest`]: the text
/// plus validated resource-link blocks.
fn prompt_blocks(text: &str, resources: &[Value]) -> Vec<Value> {
    let mut prompt = vec![serde_json::json!({ "type": "text", "text": text })];
    prompt.extend(resources.iter().cloned());
    prompt
}

/// `session/prompt` — a new prompt turn; the response carries the stop reason.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "session/prompt", response = serde_json::Value)]
pub struct PromptRequest {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub prompt: Vec<Value>,
}

/// `session/prompt` with the text and resource blocks the inbox validated.
pub fn prompt(session_id: &str, text: &str, resources: &[Value]) -> PromptRequest {
    PromptRequest {
        session_id: session_id.to_string(),
        prompt: prompt_blocks(text, resources),
    }
}

/// `_session/steering` — Claude's mid-turn steering, with the prompt-shaped
/// blocks an adapter without prompt queueing takes instead of `session/prompt`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "_session/steering", response = serde_json::Value)]
pub struct SteeringRequest {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub prompt: Vec<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// `_session/steering` asking the turn to take the queued prompt when idle.
pub fn steering(session_id: &str, text: &str, resources: &[Value]) -> SteeringRequest {
    SteeringRequest {
        session_id: session_id.to_string(),
        prompt: prompt_blocks(text, resources),
        meta: Some(serde_json::json!({
            "steering": { "idleBehavior": "promptRequired" }
        })),
    }
}

/// `session/set_mode` — switch the adapter's permission posture.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "session/set_mode", response = serde_json::Value)]
pub struct SetModeRequest {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "modeId")]
    pub mode_id: String,
}

/// `session/set_mode` for the id the adapter advertises.
pub fn set_mode(session_id: &str, mode_id: &str) -> SetModeRequest {
    SetModeRequest {
        session_id: session_id.to_string(),
        mode_id: mode_id.to_string(),
    }
}

/// `session/set_config_option` — select a model, effort, or fast value. The
/// value stays raw JSON: composer controls are select strings while flags
/// such as Codex fast mode are booleans.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "session/set_config_option", response = serde_json::Value)]
pub struct SetConfigOptionRequest {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "configId")]
    pub config_id: String,
    pub value: Value,
}

/// `session/set_config_option` for one config option.
pub fn set_config_option(
    session_id: &str,
    config_id: &str,
    value: Value,
) -> SetConfigOptionRequest {
    SetConfigOptionRequest {
        session_id: session_id.to_string(),
        config_id: config_id.to_string(),
        value,
    }
}

/// `session/cancel` — abort the live turn.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcNotification)]
#[notification(method = "session/cancel")]
pub struct CancelNotification {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// `session/cancel` for the session under cancellation.
pub fn cancel(session_id: &str) -> CancelNotification {
    CancelNotification {
        session_id: session_id.to_string(),
    }
}

/// `initialize` for the live session — the response stays raw because the
/// harness extensions the durable handshake reads (steering support, the
/// claudeCode prompt-queueing flag) are not part of the typed schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "initialize", response = serde_json::Value)]
pub struct InitializeRequest {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u16,
    #[serde(rename = "clientCapabilities")]
    pub client_capabilities: Value,
}

/// `initialize` with the same shape the hand-rolled wire builder produced.
pub fn initialize(client_capabilities: Value) -> InitializeRequest {
    InitializeRequest {
        protocol_version: 1,
        client_capabilities,
    }
}

/// `session/update` — the whole params object, raw, for the durable task's
/// tolerant DTOs to parse.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcNotification)]
#[notification(method = "session/update")]
pub struct SessionUpdateNotification(pub Value);

/// `session/request_permission` — the params object raw, answered with the
/// typed permission response the protocol defines.
#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(
    method = "session/request_permission",
    response = agent_client_protocol::v1::schema::RequestPermissionResponse
)]
pub struct PermissionRequest(pub Value);

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;

    /// The wire shapes are load-bearing against real adapters; keep them
    /// pinned so a future crate or serde change cannot drift silently.
    #[test]
    fn requests_serialize_to_the_hand_rolled_shapes() {
        let new = new_session("/w", &[], &["/extra".into()], None);
        assert_eq!(
            serde_json::to_value(&new).unwrap(),
            serde_json::json!({
                "cwd": "/w",
                "mcpServers": [],
                "additionalDirectories": ["/extra"],
            })
        );
        assert_eq!(new.method(), "session/new");

        let load = load_session("s1", "/w", &[serde_json::json!({"name": "m"})], &[], None);
        assert_eq!(
            serde_json::to_value(&load).unwrap(),
            serde_json::json!({
                "sessionId": "s1",
                "cwd": "/w",
                "mcpServers": [{"name": "m"}],
            })
        );
        assert_eq!(load.method(), "session/load");

        let prompt = prompt("s1", "hello", &[]);
        assert_eq!(
            serde_json::to_value(&prompt).unwrap(),
            serde_json::json!({
                "sessionId": "s1",
                "prompt": [{"type": "text", "text": "hello"}],
            })
        );
        assert_eq!(prompt.method(), "session/prompt");

        let steering = steering("s1", "hi", &[]);
        let value = serde_json::to_value(&steering).unwrap();
        assert_eq!(steering.method(), "_session/steering");
        assert_eq!(
            value["_meta"],
            serde_json::json!({ "steering": { "idleBehavior": "promptRequired" } })
        );
        assert_eq!(value["prompt"][0]["text"], "hi");

        let set = set_config_option("s1", "model", serde_json::json!("gpt-6-sol"));
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            serde_json::json!({
                "sessionId": "s1",
                "configId": "model",
                "value": "gpt-6-sol",
            })
        );
        assert_eq!(set.method(), "session/set_config_option");

        let fast = set_config_option("s1", "fast", serde_json::json!(true));
        assert_eq!(
            serde_json::to_value(&fast).unwrap()["value"],
            serde_json::json!(true)
        );

        let mode = set_mode("s1", "bypassPermissions");
        assert_eq!(
            serde_json::to_value(&mode).unwrap(),
            serde_json::json!({"sessionId": "s1", "modeId": "bypassPermissions"})
        );
        assert_eq!(mode.method(), "session/set_mode");

        let cancel = cancel("s1");
        assert_eq!(
            serde_json::to_value(&cancel).unwrap(),
            serde_json::json!({"sessionId": "s1"})
        );
        assert_eq!(cancel.method(), "session/cancel");
    }
}

#[cfg(test)]
mod response_shape_tests {
    use agent_client_protocol::v1::schema::{
        RequestPermissionOutcome, RequestPermissionResponse, SelectedPermissionOutcome,
    };
    use serde_json::json;

    /// The typed permission responses serialize to exactly the shapes the
    /// hand-rolled wire builders produced, which real adapters accept.
    #[test]
    fn permission_responses_keep_the_hand_rolled_shapes() {
        let selected = RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
            SelectedPermissionOutcome::new("allow-once".to_string()),
        ));
        assert_eq!(
            serde_json::to_value(&selected).unwrap(),
            json!({ "outcome": { "outcome": "selected", "optionId": "allow-once" } })
        );

        let cancelled = RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
        assert_eq!(
            serde_json::to_value(&cancelled).unwrap(),
            json!({ "outcome": { "outcome": "cancelled" } })
        );
    }
}
