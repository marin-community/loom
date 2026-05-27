//! Thin HTTP client used by every CLI subcommand except `server run`.

use anyhow::{anyhow, bail, Result};
use reqwest::Method;
use serde_json::Value;

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self {
            base: crate::endpoint::base_url(),
            http: reqwest::Client::new(),
        }
    }

    /// Base URL of the server (also the web UI origin).
    pub fn base(&self) -> &str {
        &self.base
    }

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.http.request(method, &url);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_connect() {
                anyhow!(
                    "cannot reach the weaver server at {} — start it with `weaver server start`",
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

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.send(Method::DELETE, path, None).await
    }
}
