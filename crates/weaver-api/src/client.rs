//! The typed loom REST client. A thin layer over an untyped JSON `send`: the
//! untyped `get`/`post`/`patch`/`delete` are kept for callers that pretty-print
//! raw JSON (the `loom` CLI), and the typed methods over them serialize the
//! right request DTO and deserialize the right View — the surface the Python
//! binding wraps.

use anyhow::{anyhow, bail, Result};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::dto::{
    CreateOverlookerReq, CreateReq, OverlookerView, PatchOverlookerReq, PatchSessionReq,
    RunOverlookerReq, SendReq, SessionView, TagReq,
};

/// A client for one loom server, identified by its base URL.
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    /// A client pointed at `base` (e.g. `http://127.0.0.1:7878`). loom supplies
    /// the default base from `loom::endpoint::base_url()`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Base URL of the server (also the web UI origin).
    pub fn base(&self) -> &str {
        &self.base
    }

    // -- Untyped JSON transport -------------------------------------------

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.http.request(method, &url);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_connect() {
                anyhow!(
                    "cannot reach loom at {} — start it with `loom start`",
                    self.base
                )
            } else {
                anyhow!("request to {url} failed: {e}")
            }
        })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()))
        };
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(text.as_str());
            bail!("server returned {} — {}", status.as_u16(), message);
        }
        Ok(value)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.send(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::POST, path, Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::PATCH, path, Some(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.send(Method::PUT, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.send(Method::DELETE, path, None).await
    }

    // -- Typed helpers ----------------------------------------------------

    /// Send a typed body and deserialize a typed reply, surfacing a serde error
    /// as an `anyhow` error rather than panicking.
    async fn send_typed<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<R> {
        let body = match body {
            Some(b) => Some(serde_json::to_value(b)?),
            None => None,
        };
        let value = self.send(method, path, body).await?;
        serde_json::from_value(value).map_err(|e| anyhow!("decoding response from {path}: {e}"))
    }

    async fn get_typed<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        let value = self.get(path).await?;
        serde_json::from_value(value).map_err(|e| anyhow!("decoding response from {path}: {e}"))
    }

    // -- Sessions ---------------------------------------------------------

    /// List every active session (`GET /api/sessions`).
    pub async fn list_sessions(&self) -> Result<Vec<SessionView>> {
        self.get_typed("/api/sessions").await
    }

    /// Get one session by key — id, branch id, branch name, or `repo:branch`
    /// (`GET /api/sessions/{key}`).
    pub async fn get_session(&self, key: &str) -> Result<SessionView> {
        self.get_typed(&format!("/api/sessions/{key}")).await
    }

    /// Launch a new session (`POST /api/sessions`).
    pub async fn create_session(&self, req: &CreateReq) -> Result<SessionView> {
        self.send_typed(Method::POST, "/api/sessions", Some(req))
            .await
    }

    /// Patch a session's lifecycle / branch fields (`PATCH /api/sessions/{key}`).
    pub async fn patch_session(&self, key: &str, req: &PatchSessionReq) -> Result<SessionView> {
        self.send_typed(Method::PATCH, &format!("/api/sessions/{key}"), Some(req))
            .await
    }

    /// Set (upsert) a tag on a session
    /// (`PUT /api/sessions/{key}/tags/{tag_key}`). For a loud key (`attention` |
    /// `triage`) `value` is `attention` | `blocked`; use [`Client::clear_tag`] to
    /// return to calm rather than setting an `ok` value.
    pub async fn set_tag(
        &self,
        key: &str,
        tag_key: &str,
        value: &str,
        note: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        let req = TagReq {
            value: value.to_string(),
            note: note.to_string(),
            by: by.map(str::to_string),
        };
        self.send_typed(
            Method::PUT,
            &format!("/api/sessions/{key}/tags/{tag_key}"),
            Some(&req),
        )
        .await
    }

    /// Clear a tag on a session (`DELETE /api/sessions/{key}/tags/{tag_key}`) —
    /// how a loud axis returns to calm (`ok`).
    pub async fn clear_tag(&self, key: &str, tag_key: &str) -> Result<SessionView> {
        let value = self
            .delete(&format!("/api/sessions/{key}/tags/{tag_key}"))
            .await?;
        serde_json::from_value(value)
            .map_err(|e| anyhow!("decoding response from /api/sessions/{key}/tags/{tag_key}: {e}"))
    }

    /// Stamp an overlooker's mark on a session — the `triage` tag. A convenience
    /// over [`Client::set_tag`] / [`Client::clear_tag`] that keeps the `mark`
    /// capability name: a `level` of `attention`/`blocked` sets the tag, an empty
    /// `level` (or `ok`) clears it.
    pub async fn mark(
        &self,
        key: &str,
        level: &str,
        note: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        if level.is_empty() || level == "ok" {
            self.clear_tag(key, weaver_core::tags::TRIAGE_KEY).await
        } else {
            self.set_tag(key, weaver_core::tags::TRIAGE_KEY, level, note, by)
                .await
        }
    }

    /// Type a message into a session's agent pane, submitting it by default
    /// (`POST /api/sessions/{key}/send`). Returns the raw `{sent, submitted}`.
    pub async fn nudge(&self, key: &str, req: &SendReq) -> Result<Value> {
        let body = serde_json::to_value(req)?;
        self.post(&format!("/api/sessions/{key}/send"), body).await
    }

    /// Send a break (Escape) to interrupt the agent's current turn
    /// (`POST /api/sessions/{key}/interrupt`).
    pub async fn interrupt(&self, key: &str) -> Result<Value> {
        self.post(&format!("/api/sessions/{key}/interrupt"), Value::Null)
            .await
    }

    /// Capture the session's tmux pane as plain text, with `lines` of extra
    /// scrollback above the visible screen (`GET /api/sessions/{key}/preview`).
    pub async fn preview(&self, key: &str, lines: usize) -> Result<String> {
        let value = self
            .get(&format!("/api/sessions/{key}/preview?lines={lines}"))
            .await?;
        Ok(value
            .get("screen")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// The worktree file tree + change map vs the diff base
    /// (`GET /api/sessions/{key}/tree`). Returned as raw JSON — the shape is a
    /// loose `{files, changed, base}` the dashboard assembles client-side.
    pub async fn diff(&self, key: &str) -> Result<Value> {
        self.get(&format!("/api/sessions/{key}/tree")).await
    }

    // -- Overlookers ------------------------------------------------------

    /// List every overlooker (`GET /api/overlookers`).
    pub async fn list_overlookers(&self) -> Result<Vec<OverlookerView>> {
        self.get_typed("/api/overlookers").await
    }

    /// Get one overlooker by id or name (`GET /api/overlookers/{key}`).
    pub async fn get_overlooker(&self, key: &str) -> Result<OverlookerView> {
        self.get_typed(&format!("/api/overlookers/{key}")).await
    }

    /// Register an overlooker (`POST /api/overlookers`).
    pub async fn create_overlooker(&self, req: &CreateOverlookerReq) -> Result<OverlookerView> {
        self.send_typed(Method::POST, "/api/overlookers", Some(req))
            .await
    }

    /// Patch an overlooker (`PATCH /api/overlookers/{key}`).
    pub async fn patch_overlooker(
        &self,
        key: &str,
        req: &PatchOverlookerReq,
    ) -> Result<OverlookerView> {
        self.send_typed(Method::PATCH, &format!("/api/overlookers/{key}"), Some(req))
            .await
    }

    /// Delete an overlooker (`DELETE /api/overlookers/{key}`).
    pub async fn delete_overlooker(&self, key: &str) -> Result<Value> {
        self.delete(&format!("/api/overlookers/{key}")).await
    }

    /// Fire a round now and return the raw `{run_id, outcome, summary}`
    /// (`POST /api/overlookers/{key}/run`).
    pub async fn run_overlooker(&self, key: &str, req: &RunOverlookerReq) -> Result<Value> {
        let body = serde_json::to_value(req)?;
        self.post(&format!("/api/overlookers/{key}/run"), body)
            .await
    }
}
