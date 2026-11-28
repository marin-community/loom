//! One ACP session that lists a harness's models and reads each model's options.
//!
//! The session rides the `agent-client-protocol` crate: its engine owns frame
//! reading, request/response correlation, and the ordered dispatch loop, over a
//! [`ByteStreams`] transport wrapping the spawned adapter's stdio. The
//! *payloads* stay raw (`super::requests`) because harness config options are
//! not strictly typed across harnesses. The session is ephemeral: initialize,
//! `session/new`, then `session/set_config_option` for the models this
//! reconcile still needs. Per-model failures are recorded and the loop
//! continues. A probe runs no prompts, so an adapter permission request is
//! cancelled; every other adapter-initiated request is answered by the
//! engine's method-not-found default.

use std::process::Stdio;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    ClientCapabilities, InitializeRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse,
};
use agent_client_protocol::{ByteStreams, Client as AcpClient};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::requests;
use crate::catalog::selectors::{self, current_model, model_choices, model_option_id};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct ListedModel {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Handshake {
    pub version: String,
    pub models: Vec<ListedModel>,
    pub default_model: Option<String>,
    pub default_options: Vec<Value>,
    /// Id of the model config option on this session. Adapters do not all
    /// spell it `model`.
    pub model_config_id: String,
}

/// What one reconcile needs from a probe. Tests supply a scripted session;
/// production opens a live adapter.
pub trait ProbeSession {
    fn handshake(&self) -> &Handshake;
    fn fetch_model(
        &mut self,
        model_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<Value>>> + Send;
}

/// One live catalogue probe: the connection runs on its own task over the
/// adapter's stdio, and [`ProbeSession::fetch_model`] exchanges messages with
/// it. Dropping the session closes the request channel (ending the
/// connection) and kills the child.
pub struct LiveSession {
    handshake: Handshake,
    requests: mpsc::UnboundedSender<String>,
    replies: mpsc::UnboundedReceiver<Result<Vec<Value>, String>>,
    child: Child,
    pace: Duration,
}

impl LiveSession {
    pub async fn open(command: &str, harness: &str, env: &[(String, String)]) -> Result<Self> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .envs(
                env.iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {harness} catalogue probe"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("catalogue probe has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("catalogue probe has no stdout"))?;
        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

        let (requests_tx, requests_rx) = mpsc::unbounded_channel::<String>();
        let (replies_tx, replies_rx) = mpsc::unbounded_channel::<Result<Vec<Value>, String>>();
        let (handshake_tx, handshake_rx) = oneshot::channel::<Result<Handshake>>();
        let harness_meta = selectors::initialize_meta(harness);
        let pace = pace_for(harness);
        let command_fallback = command.to_string();

        tokio::spawn(async move {
            let connect = AcpClient
                .v1()
                .on_receive_request(
                    async |request: RequestPermissionRequest, responder, _cx| {
                        // A probe runs no prompts, so a permission request has
                        // no one to ask. Cancel it rather than guess.
                        let _ = request;
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Cancelled,
                        ))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(transport, async move |cx| {
                    probe_session(
                        cx,
                        requests_rx,
                        replies_tx,
                        handshake_tx,
                        harness_meta,
                        &command_fallback,
                    )
                    .await;
                    Ok(())
                });
            if let Err(error) = connect.await {
                tracing::debug!(%error, "catalogue probe connection ended");
            }
        });

        let handshake = timeout(REQUEST_TIMEOUT.saturating_mul(2), handshake_rx)
            .await
            .map_err(|_| anyhow!("timed out waiting for the {harness} catalogue probe handshake"))?
            .map_err(|_| anyhow!("channel dropped before the probe handshake"))??;
        Ok(Self {
            handshake,
            requests: requests_tx,
            replies: replies_rx,
            child,
            pace,
        })
    }
}

/// Drive one probe session on the connection: initialize, `session/new`, then
/// one `session/set_config_option` per requested model, reporting the
/// handshake once and each model's options (or failure) as they resolve.
/// A setup failure is reported through the handshake channel so
/// [`LiveSession::open`] sees it; after the handshake, per-model errors ride
/// the replies channel.
async fn probe_session(
    cx: agent_client_protocol::v1::ConnectionTo<agent_client_protocol::Agent>,
    mut requests_rx: mpsc::UnboundedReceiver<String>,
    replies_tx: mpsc::UnboundedSender<Result<Vec<Value>, String>>,
    handshake_tx: oneshot::Sender<Result<Handshake>>,
    harness_meta: Option<Value>,
    command: &str,
) {
    let (handshake, session_id) = match probe_handshake(&cx, harness_meta.as_ref(), command).await {
        Ok(found) => found,
        Err(error) => {
            let _ = handshake_tx.send(Err(error));
            return;
        }
    };
    let model_config_id = handshake.model_config_id.clone();
    let _ = handshake_tx.send(Ok(handshake));

    while let Some(model_id) = requests_rx.recv().await {
        let reply = match timeout(
            REQUEST_TIMEOUT,
            cx.send_request(requests::set_config_option(
                &session_id,
                &model_config_id,
                Value::String(model_id.clone()),
            ))
            .block_task(),
        )
        .await
        {
            Err(_) => Err(format!("timed out waiting for options of {model_id}")),
            Ok(Err(error)) => Err(format!("session/set_config_option failed: {error}")),
            Ok(Ok(result)) => {
                let options = config_options_of(&result);
                if options.is_empty() {
                    Err("session/set_config_option returned no configOptions".to_string())
                } else {
                    Ok(options)
                }
            }
        };
        if replies_tx.send(reply).is_err() {
            // The reconcile dropped the session; stop asking for models.
            break;
        }
    }
    // The request channel closed with the session; the connection ends here.
}

/// The capabilities a probe or transient session advertises: no fs, no
/// terminal — only the harness's initialize `_meta` when it opted into one.
pub(crate) fn client_capabilities(meta: Option<&Value>) -> ClientCapabilities {
    let mut capabilities = ClientCapabilities::new();
    if let Some(meta) = meta.and_then(Value::as_object) {
        capabilities = capabilities.meta(meta.clone());
    }
    capabilities
}

/// Initialize and open one `session/new`, returning the handshake it produced
/// plus the adapter's session id.
async fn probe_handshake(
    cx: &agent_client_protocol::v1::ConnectionTo<agent_client_protocol::Agent>,
    harness_meta: Option<&Value>,
    command: &str,
) -> Result<(Handshake, String), anyhow::Error> {
    let init = timeout(
        REQUEST_TIMEOUT,
        cx.send_request(
            InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(client_capabilities(harness_meta)),
        )
        .block_task(),
    )
    .await
    .map_err(|_| anyhow!("timed out waiting for the initialize response"))??;
    let trimmed_version = init
        .agent_info
        .as_ref()
        .map(|info| info.version.trim().to_string())
        .unwrap_or_default();
    let version = if trimmed_version.is_empty() {
        command.to_string()
    } else {
        trimmed_version
    };

    let cwd = std::env::temp_dir();
    let created = timeout(
        REQUEST_TIMEOUT,
        cx.send_request(requests::new_session(
            &cwd.to_string_lossy(),
            &[],
            &[],
            None,
        ))
        .block_task(),
    )
    .await
    .map_err(|_| anyhow!("timed out waiting for the session/new response"))??;
    let session_id = created
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("session/new returned no sessionId"))?
        .to_string();
    let config_options = config_options_of(&created);
    let models = listed_models(&created, &config_options);
    if models.is_empty() {
        anyhow::bail!("session/new advertised no models");
    }
    Ok((
        Handshake {
            version,
            models,
            default_model: current_model(&config_options),
            model_config_id: model_option_id(&config_options)
                .unwrap_or("model")
                .to_string(),
            default_options: config_options,
        },
        session_id,
    ))
}

impl ProbeSession for LiveSession {
    fn handshake(&self) -> &Handshake {
        &self.handshake
    }

    async fn fetch_model(&mut self, model_id: &str) -> Result<Vec<Value>> {
        if !self.pace.is_zero() {
            tokio::time::sleep(self.pace).await;
        }
        self.requests
            .send(model_id.to_string())
            .map_err(|_| anyhow!("catalogue probe closed"))?;
        let reply = timeout(REQUEST_TIMEOUT, self.replies.recv())
            .await
            .map_err(|_| anyhow!("timed out waiting for options of {model_id}"))?
            .ok_or_else(|| anyhow!("catalogue probe closed"))?
            .map_err(|error| anyhow!(error))?;
        if reply.is_empty() {
            anyhow::bail!("session/set_config_option returned no configOptions");
        }
        Ok(reply)
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub fn pace_for(harness: &str) -> Duration {
    crate::harnesses::builtin_for_id(harness)
        .map(|harness| harness.probe_pace())
        .unwrap_or(Duration::ZERO)
}

fn config_options_of(result: &Value) -> Vec<Value> {
    result
        .get("configOptions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn listed_models(result: &Value, config_options: &[Value]) -> Vec<ListedModel> {
    let from_options = model_choices(config_options)
        .into_iter()
        .map(|choice| ListedModel {
            id: choice.id,
            label: choice.label,
        })
        .collect::<Vec<_>>();
    if !from_options.is_empty() {
        return from_options;
    }
    result
        .get("models")
        .and_then(|models| models.get("availableModels"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("modelId").and_then(Value::as_str)?.to_string();
            let label = model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            Some(ListedModel { id, label })
        })
        .collect()
}

/// A probe whose handshake and per-model results are fixed. `Err` entries are
/// per-model failures; a missing id is also a failure.
#[cfg(test)]
pub struct ScriptedSession {
    pub handshake: Handshake,
    pub fetched: std::collections::HashMap<String, Result<Vec<Value>, String>>,
}

#[cfg(test)]
impl ProbeSession for ScriptedSession {
    fn handshake(&self) -> &Handshake {
        &self.handshake
    }

    async fn fetch_model(&mut self, model_id: &str) -> Result<Vec<Value>> {
        match self.fetched.get(model_id) {
            Some(Ok(options)) => Ok(options.clone()),
            Some(Err(error)) => Err(anyhow!(error.clone())),
            None => Err(anyhow!("no scripted options for {model_id}")),
        }
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Manual smoke test against the suite's fake ACP agent; run with
    /// `cargo test -p loom-agent --lib -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn probe_opens_the_fake_adapter() {
        let cmd = format!(
            "node {}/../loom/tests/fixtures/fake-acp-agent.mjs",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut session = LiveSession::open(&cmd, "claude", &[]).await.expect("open");
        println!("handshake: {:?}", session.handshake());
        let opts = session.fetch_model("fake-deep").await.expect("fetch");
        println!("options: {opts:#?}");
    }

    /// Manual smoke test against each real harness adapter, using the same
    /// command resolution a live reconcile would. Requires the harness CLIs
    /// and credentials on the machine it runs on; run with
    /// `cargo test -p loom-agent --lib live_adapter -- --ignored --nocapture`.
    async fn probe_real_harness(harness: &'static str, command: &str) {
        let mut session = match LiveSession::open(command, harness, &[]).await {
            Ok(session) => session,
            Err(error) => {
                println!("{harness}: open failed: {error:#}");
                return;
            }
        };
        let handshake = session.handshake().clone();
        println!(
            "{harness}: version {}, {} models: {:?}",
            handshake.version,
            handshake.models.len(),
            handshake
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
        );
        let first = &handshake.models[0];
        match session.fetch_model(&first.id).await {
            Ok(options) => println!(
                "{harness}: {} -> {} options: {}",
                first.id,
                options.len(),
                serde_json::to_string(&options).unwrap_or_default()
            ),
            Err(error) => println!("{harness}: fetch {} failed: {error:#}", first.id),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_adapter_claude() {
        probe_real_harness(
            "claude",
            "command -v claude-agent-acp >/dev/null 2>&1 && exec claude-agent-acp; exec npx --yes @agentclientprotocol/claude-agent-acp",
        )
        .await;
    }

    #[tokio::test]
    #[ignore]
    async fn live_adapter_codex() {
        probe_real_harness(
            "codex",
            "command -v codex-acp >/dev/null 2>&1 && exec codex-acp; exec npx --yes @agentclientprotocol/codex-acp",
        )
        .await;
    }

    #[tokio::test]
    #[ignore]
    async fn live_adapter_opencode() {
        probe_real_harness("opencode", "opencode acp").await;
    }

    #[tokio::test]
    #[ignore]
    async fn live_adapter_cursor() {
        probe_real_harness("cursor-agent", "cursor-agent acp").await;
    }
}
