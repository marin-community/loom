//! One ACP session that lists a harness's models and reads each model's options.
//!
//! The session is ephemeral: initialize, `session/new`, then
//! `session/set_config_option` for the models this reconcile still needs.
//! Per-model failures are recorded and the loop continues.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::time::timeout;

use super::wire;
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

pub struct LiveSession {
    handshake: Handshake,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
    session_id: String,
    pace: Duration,
}

impl LiveSession {
    pub async fn open(command: &str, harness: &str, env: &[(String, String)]) -> Result<Self> {
        let mut child = tokio::process::Command::new("sh")
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
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("catalogue probe has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("catalogue probe has no stdout"))?;
        let mut lines = BufReader::new(stdout).lines();

        let init_id = 1u64;
        write_frame(
            &mut stdin,
            &wire::request_line(
                init_id,
                wire::method::INITIALIZE,
                wire::initialize_params(harness),
            ),
        )
        .await?;
        let initialized = read_response(&mut lines, &mut stdin, init_id).await?;
        let version = initialized
            .get("agentInfo")
            .and_then(|info| info.get("version"))
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
            .unwrap_or(command)
            .to_string();

        let new_id = 2u64;
        let cwd = std::env::temp_dir();
        write_frame(
            &mut stdin,
            &wire::request_line(
                new_id,
                wire::method::SESSION_NEW,
                wire::new_session_params(&cwd.to_string_lossy(), &[], &[], None),
            ),
        )
        .await?;
        let created = read_response(&mut lines, &mut stdin, new_id).await?;
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
        Ok(Self {
            handshake: Handshake {
                version,
                models,
                default_model: current_model(&config_options),
                model_config_id: model_option_id(&config_options)
                    .unwrap_or("model")
                    .to_string(),
                default_options: config_options,
            },
            child,
            stdin,
            lines,
            next_id: new_id,
            session_id,
            pace: pace_for(harness),
        })
    }
}

impl ProbeSession for LiveSession {
    fn handshake(&self) -> &Handshake {
        &self.handshake
    }

    async fn fetch_model(&mut self, model_id: &str) -> Result<Vec<Value>> {
        if !self.pace.is_zero() {
            tokio::time::sleep(self.pace).await;
        }
        self.next_id += 1;
        let id = self.next_id;
        write_frame(
            &mut self.stdin,
            &wire::request_line(
                id,
                wire::method::SESSION_SET_CONFIG_OPTION,
                wire::set_config_option_params(
                    &self.session_id,
                    &self.handshake.model_config_id,
                    Value::String(model_id.to_string()),
                ),
            ),
        )
        .await?;
        let response = read_response(&mut self.lines, &mut self.stdin, id).await?;
        let options = config_options_of(&response);
        if options.is_empty() {
            anyhow::bail!("session/set_config_option returned no configOptions");
        }
        Ok(options)
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

pub(crate) fn client_capabilities(harness: &str) -> Value {
    match selectors::initialize_meta(harness) {
        Some(meta) => serde_json::json!({ "_meta": meta }),
        None => serde_json::json!({}),
    }
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

/// Write one ACP line (already newline-terminated) and flush it.
async fn write_frame(stdin: &mut ChildStdin, line: &[u8]) -> Result<()> {
    stdin.write_all(line).await.context("writing ACP frame")?;
    stdin.flush().await.ok();
    Ok(())
}

async fn read_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: &mut ChildStdin,
    id: u64,
) -> Result<Value> {
    let wait = async {
        loop {
            let Some(line) = lines.next_line().await? else {
                anyhow::bail!("catalogue probe closed its stdout");
            };
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame.get("method").is_some() {
                if let Some(request_id) = frame.get("id").cloned() {
                    // A probe runs no prompts, so a permission request has no
                    // one to ask. Cancel it rather than reply with a shapeless
                    // empty result, and name any other request as unsupported.
                    let reply = if frame.get("method").and_then(Value::as_str)
                        == Some(wire::method::SESSION_REQUEST_PERMISSION)
                    {
                        wire::response_line(&request_id, wire::permission_cancelled())
                    } else {
                        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
                        wire::error_line(
                            &request_id,
                            wire::METHOD_NOT_FOUND,
                            &format!("{method} is not supported by a catalogue probe"),
                        )
                    };
                    stdin.write_all(&reply).await.context("writing ACP reply")?;
                    stdin.flush().await.ok();
                }
                continue;
            }
            if frame.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = frame.get("error") {
                    anyhow::bail!("ACP request {id} failed: {error}");
                }
                return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    };
    timeout(REQUEST_TIMEOUT, wait)
        .await
        .map_err(|_| anyhow!("timed out waiting for ACP response {id}"))?
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
