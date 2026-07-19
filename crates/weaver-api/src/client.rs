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
    AnchorDto, ArtifactMeta, ArtifactUpsertReq, ArtifactView, BranchStatusReq, BranchView,
    CommentDto, CreateEventReq, CreateIssueReq, CreateRepoIssueReq, CreateReq, CreateTokenReq,
    CreateWatchReq, CreatedTokenView, HandoffReq, IssueView, NewCommentBody, NewThreadBody,
    PatchIssueReq, PatchSessionReq, PatchWatchReq, RunWatchReq, SendReq, SessionView,
    SettingsEnvelope, TagReq, ThreadDto, TokenView, WatchView,
};

/// A client for one loom server, identified by its base URL.
pub struct Client {
    base: String,
    http: reqwest::Client,
    /// Optional bearer token sent on every request (the `LOOM_TOKEN` for a
    /// remote or non-loopback-trusted server). `None` relies on loopback trust.
    token: Option<String>,
}

impl Client {
    /// A client pointed at `base` (e.g. `http://127.0.0.1:7878`). loom supplies
    /// the default base from `loom::endpoint::base_url()`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            http: reqwest::Client::new(),
            token: None,
        }
    }

    /// Attach a bearer token (sent as `Authorization: Bearer …`). A `None` or
    /// empty value leaves the client unauthenticated. loom's `client::default`
    /// wires this from `$LOOM_TOKEN` / the machine token.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.trim().is_empty());
        self
    }

    /// Base URL of the server (also the web UI origin).
    pub fn base(&self) -> &str {
        &self.base
    }

    // -- URL construction ---------------------------------------------------

    /// Percent-encode a value embedded as a single URL path segment. Branch
    /// keys in particular are often `repo_root:branch` — a real repo root is
    /// an absolute path full of `/`, which would otherwise split into extra
    /// path segments the router never matches.
    fn seg(s: &str) -> String {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    }

    // -- Untyped JSON transport -------------------------------------------

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.http.request(method, &url);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_connect() {
                anyhow!(
                    "cannot reach loom at {} — no active loom session (start the server with `loom server start`, or check $WEAVER_API)",
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
        self.get_typed(&format!("/api/sessions/{}", Self::seg(key)))
            .await
    }

    /// Launch a new session (`POST /api/sessions`).
    pub async fn create_session(&self, req: &CreateReq) -> Result<SessionView> {
        self.send_typed(Method::POST, "/api/sessions", Some(req))
            .await
    }

    /// Patch a session's lifecycle / branch fields (`PATCH /api/sessions/{key}`).
    pub async fn patch_session(&self, key: &str, req: &PatchSessionReq) -> Result<SessionView> {
        self.send_typed(
            Method::PATCH,
            &format!("/api/sessions/{}", Self::seg(key)),
            Some(req),
        )
        .await
    }

    /// Replace the provider behind a live ACP session while preserving the
    /// loom session, worktree, branch, and canonical journal.
    pub async fn handoff_session(&self, key: &str, req: &HandoffReq) -> Result<SessionView> {
        self.send_typed(
            Method::POST,
            &format!("/api/sessions/{}/handoff", Self::seg(key)),
            Some(req),
        )
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
            &format!(
                "/api/sessions/{}/tags/{}",
                Self::seg(key),
                Self::seg(tag_key)
            ),
            Some(&req),
        )
        .await
    }

    /// Clear a tag on a session (`DELETE /api/sessions/{key}/tags/{tag_key}`) —
    /// how a loud axis returns to calm (`ok`). `by` attributes the clear on
    /// the audit event (a watch name); the server defaults `manual`.
    pub async fn clear_tag(
        &self,
        key: &str,
        tag_key: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        let query = by
            .map(|b| {
                format!(
                    "?by={}",
                    percent_encoding::utf8_percent_encode(b, percent_encoding::NON_ALPHANUMERIC)
                )
            })
            .unwrap_or_default();
        let value = self
            .delete(&format!(
                "/api/sessions/{}/tags/{}{query}",
                Self::seg(key),
                Self::seg(tag_key)
            ))
            .await?;
        serde_json::from_value(value)
            .map_err(|e| anyhow!("decoding response from /api/sessions/{key}/tags/{tag_key}: {e}"))
    }

    /// Stamp a watch's mark on a session — the `triage` tag. A convenience
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
            self.clear_tag(key, weaver_core::tags::TRIAGE_KEY, by).await
        } else {
            self.set_tag(key, weaver_core::tags::TRIAGE_KEY, level, note, by)
                .await
        }
    }

    /// Type a message into a session's agent pane, submitting it by default
    /// (`POST /api/sessions/{key}/send`). Returns the raw `{sent, submitted}`.
    pub async fn nudge(&self, key: &str, req: &SendReq) -> Result<Value> {
        let body = serde_json::to_value(req)?;
        self.post(&format!("/api/sessions/{}/send", Self::seg(key)), body)
            .await
    }

    /// Send a break (Escape) to interrupt the agent's current turn
    /// (`POST /api/sessions/{key}/interrupt`).
    pub async fn interrupt(&self, key: &str) -> Result<Value> {
        self.post(
            &format!("/api/sessions/{}/interrupt", Self::seg(key)),
            Value::Null,
        )
        .await
    }

    /// Capture the session's terminal pane as plain text, with `lines` of extra
    /// scrollback above the visible screen (`GET /api/sessions/{key}/preview`).
    pub async fn preview(&self, key: &str, lines: usize) -> Result<String> {
        let value = self
            .get(&format!(
                "/api/sessions/{}/preview?lines={lines}",
                Self::seg(key)
            ))
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
        self.get(&format!("/api/sessions/{}/tree", Self::seg(key)))
            .await
    }

    /// Recent events for a branch, newest first, capped at 200 server-side
    /// (`GET /api/sessions/{key}/log`). Despite the URL, `key` may be a branch
    /// id, `repo:branch`, or unambiguous id prefix — no live session required.
    pub async fn branch_log(&self, key: &str) -> Result<Vec<weaver_core::events::Event>> {
        self.get_typed(&format!("/api/sessions/{}/log", Self::seg(key)))
            .await
    }

    // -- Branches -----------------------------------------------------------

    /// Get one branch by id, `repo:branch`, or unambiguous id prefix — no live
    /// session required (`GET /api/branches/{key}`).
    pub async fn get_branch(&self, key: &str) -> Result<BranchView> {
        self.get_typed(&format!("/api/branches/{}", Self::seg(key)))
            .await
    }

    /// Set the agent's attention level and current-state message in one call
    /// (`POST /api/branches/{key}/status`). `level` is `ok` | `attention` |
    /// `blocked`; an empty `message` leaves the previous one in place.
    pub async fn set_branch_status(
        &self,
        key: &str,
        level: &str,
        message: &str,
    ) -> Result<BranchView> {
        let req = BranchStatusReq {
            level: level.to_string(),
            message: (!message.is_empty()).then(|| message.to_string()),
        };
        self.send_typed(
            Method::POST,
            &format!("/api/branches/{}/status", Self::seg(key)),
            Some(&req),
        )
        .await
    }

    /// Set (upsert) a tag on a branch, no live session required
    /// (`PUT /api/branches/{key}/tags/{tag_key}`).
    pub async fn set_branch_tag(
        &self,
        key: &str,
        tag_key: &str,
        value: &str,
        note: &str,
        by: &str,
    ) -> Result<BranchView> {
        let req = TagReq {
            value: value.to_string(),
            note: note.to_string(),
            by: Some(by.to_string()),
        };
        self.send_typed(
            Method::PUT,
            &format!(
                "/api/branches/{}/tags/{}",
                Self::seg(key),
                Self::seg(tag_key)
            ),
            Some(&req),
        )
        .await
    }

    /// Clear a tag on a branch, no live session required
    /// (`DELETE /api/branches/{key}/tags/{tag_key}`).
    pub async fn clear_branch_tag(&self, key: &str, tag_key: &str, by: &str) -> Result<BranchView> {
        let query = Self::seg(by);
        let value = self
            .delete(&format!(
                "/api/branches/{}/tags/{}?by={query}",
                Self::seg(key),
                Self::seg(tag_key)
            ))
            .await?;
        serde_json::from_value(value)
            .map_err(|e| anyhow!("decoding response from /api/branches/{key}/tags/{tag_key}: {e}"))
    }

    /// Append a raw event row to a branch's log — the escape hatch for an
    /// event kind with no dedicated mutating route (e.g. an agent hook)
    /// (`POST /api/branches/{key}/events`).
    pub async fn record_branch_event(&self, key: &str, kind: &str, data: Value) -> Result<Value> {
        let req = CreateEventReq {
            kind: kind.to_string(),
            data,
        };
        let body = serde_json::to_value(&req)?;
        self.post(&format!("/api/branches/{}/events", Self::seg(key)), body)
            .await
    }

    // -- Branch-scoped artifacts ---------------------------------------------
    //
    // Unlike the session-scoped `/api/sessions/{key}/artifacts*` routes (which
    // 404 without a live session — the dashboard's normal case), these work
    // against the branch row directly: what the `weaver artifact` CLI needs,
    // since it may target a branch with no active session.

    /// List a branch's artifacts — its own plus repo-shared, or (`repo: true`)
    /// every artifact in the repo regardless of scope
    /// (`GET /api/branches/{key}/artifacts`).
    pub async fn list_branch_artifacts(&self, key: &str, repo: bool) -> Result<Vec<ArtifactMeta>> {
        self.get_typed(&format!(
            "/api/branches/{}/artifacts?repo={repo}",
            Self::seg(key)
        ))
        .await
    }

    /// Fetch an artifact's content. By default resolves branch-scoped first
    /// then repo-shared (what `show` displays); `repo: true` targets the
    /// repo-shared row of this name specifically. `rev` selects a revision;
    /// `None` is the latest (`GET /api/branches/{key}/artifacts/{name}`).
    pub async fn get_branch_artifact(
        &self,
        key: &str,
        name: &str,
        rev: Option<i64>,
        repo: bool,
    ) -> Result<ArtifactView> {
        let rev = match rev {
            Some(r) => format!("&rev={r}"),
            None => String::new(),
        };
        self.get_typed(&format!(
            "/api/branches/{}/artifacts/{}?repo={repo}{rev}",
            Self::seg(key),
            Self::seg(name)
        ))
        .await
    }

    /// Write a new revision of an artifact, creating it if absent
    /// (`PUT /api/branches/{key}/artifacts/{name}`) — unlike the session-scoped
    /// `PUT`, which requires the artifact to already exist.
    pub async fn write_branch_artifact(
        &self,
        key: &str,
        name: &str,
        req: &ArtifactUpsertReq,
    ) -> Result<ArtifactView> {
        self.send_typed(
            Method::PUT,
            &format!(
                "/api/branches/{}/artifacts/{}",
                Self::seg(key),
                Self::seg(name)
            ),
            Some(req),
        )
        .await
    }

    /// The dashboard deep-link for a branch artifact, resolved server-side
    /// (`GET /api/branches/{key}/artifacts/{name}/url`) so it carries the
    /// externally-visible origin (`auth.base_url`, else the request Host) rather
    /// than the loopback/wildcard address the agent dials — a `0.0.0.0` link is
    /// useless to whoever reads it. See `loom session url` for the same pattern.
    pub async fn branch_artifact_url(&self, key: &str, name: &str) -> Result<String> {
        let v = self
            .get(&format!(
                "/api/branches/{}/artifacts/{}/url",
                Self::seg(key),
                Self::seg(name)
            ))
            .await?;
        v.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("server returned no url"))
    }

    /// Delete an artifact and its whole revision history. `repo: true` targets
    /// the repo-shared row of this name rather than the branch-scoped one
    /// (`DELETE /api/branches/{key}/artifacts/{name}`).
    pub async fn delete_branch_artifact(&self, key: &str, name: &str, repo: bool) -> Result<Value> {
        self.delete(&format!(
            "/api/branches/{}/artifacts/{}?repo={repo}",
            Self::seg(key),
            Self::seg(name)
        ))
        .await
    }

    // -- Branch-scoped discussion ---------------------------------------------
    //
    // The twin of loom's session-scoped thread routes, for `weaver artifact
    // comment/resolve/threads`, which — like every other `weaver` command —
    // needs no live session.

    /// Every thread on an artifact, open and resolved, each with its comments
    /// (`GET /api/branches/{key}/artifacts/{name}/threads`).
    pub async fn list_branch_threads(&self, key: &str, name: &str) -> Result<Vec<ThreadDto>> {
        self.get_typed(&format!(
            "/api/branches/{}/artifacts/{}/threads",
            Self::seg(key),
            Self::seg(name)
        ))
        .await
    }

    /// Open a new thread anchored to a quoted span, seeded with its first
    /// comment (`POST /api/branches/{key}/artifacts/{name}/threads`).
    pub async fn create_branch_thread(
        &self,
        key: &str,
        name: &str,
        base_rev: i64,
        anchor: AnchorDto,
        body: &str,
    ) -> Result<ThreadDto> {
        let req = NewThreadBody {
            base_rev,
            anchor,
            body: body.to_string(),
        };
        self.send_typed(
            Method::POST,
            &format!(
                "/api/branches/{}/artifacts/{}/threads",
                Self::seg(key),
                Self::seg(name)
            ),
            Some(&req),
        )
        .await
    }

    /// Append a reply to an existing thread
    /// (`POST /api/branches/{key}/artifacts/{name}/threads/{tid}/comments`).
    pub async fn add_branch_thread_comment(
        &self,
        key: &str,
        name: &str,
        thread_id: i64,
        body: &str,
    ) -> Result<CommentDto> {
        let req = NewCommentBody {
            body: body.to_string(),
        };
        self.send_typed(
            Method::POST,
            &format!(
                "/api/branches/{}/artifacts/{}/threads/{thread_id}/comments",
                Self::seg(key),
                Self::seg(name)
            ),
            Some(&req),
        )
        .await
    }

    /// Mark a thread resolved
    /// (`POST /api/branches/{key}/artifacts/{name}/threads/{tid}/resolve`).
    pub async fn resolve_branch_thread(
        &self,
        key: &str,
        name: &str,
        thread_id: i64,
    ) -> Result<Value> {
        self.post(
            &format!(
                "/api/branches/{}/artifacts/{}/threads/{thread_id}/resolve",
                Self::seg(key),
                Self::seg(name)
            ),
            Value::Null,
        )
        .await
    }

    // -- Issues ---------------------------------------------------------------

    /// Create an issue claimed by a branch (`POST /api/branches/{key}/issues`).
    pub async fn create_branch_issue(&self, key: &str, req: &CreateIssueReq) -> Result<IssueView> {
        self.send_typed(
            Method::POST,
            &format!("/api/branches/{}/issues", Self::seg(key)),
            Some(req),
        )
        .await
    }

    /// Create an unclaimed repo-level backlog item
    /// (`POST /api/repos/issues`).
    pub async fn create_repo_issue(&self, req: &CreateRepoIssueReq) -> Result<IssueView> {
        self.send_typed(Method::POST, "/api/repos/issues", Some(req))
            .await
    }

    /// Every issue in a repo (`scope: "repo"`), or just the unclaimed backlog
    /// (`scope: "backlog"`) — the one fetch every `weaver issue ls` view
    /// partitions client-side (`GET /api/repos/issues`).
    pub async fn list_repo_issues(
        &self,
        repo_root: &str,
        scope: &str,
        all: bool,
    ) -> Result<Vec<IssueView>> {
        let repo_root =
            percent_encoding::utf8_percent_encode(repo_root, percent_encoding::NON_ALPHANUMERIC);
        self.get_typed(&format!(
            "/api/repos/issues?repo_root={repo_root}&scope={scope}&all={all}"
        ))
        .await
    }

    /// Get one issue by id (`GET /api/issues/{id}`).
    pub async fn get_issue(&self, id: i64) -> Result<IssueView> {
        self.get_typed(&format!("/api/issues/{id}")).await
    }

    /// Patch an issue's title/body/status (`PATCH /api/issues/{id}`).
    pub async fn patch_issue(&self, id: i64, req: &PatchIssueReq) -> Result<IssueView> {
        self.send_typed(Method::PATCH, &format!("/api/issues/{id}"), Some(req))
            .await
    }

    /// Delete an issue (`DELETE /api/issues/{id}`).
    pub async fn delete_issue(&self, id: i64) -> Result<Value> {
        self.delete(&format!("/api/issues/{id}")).await
    }

    /// Set (upsert) a free-form label on an issue
    /// (`PUT /api/issues/{id}/tags/{key}`). Issue tags carry no
    /// `attention`/`triage` ladder — every key is a quiet annotation.
    pub async fn set_issue_tag(
        &self,
        id: i64,
        key: &str,
        value: &str,
        note: &str,
        by: &str,
    ) -> Result<IssueView> {
        let req = TagReq {
            value: value.to_string(),
            note: note.to_string(),
            by: Some(by.to_string()),
        };
        self.send_typed(
            Method::PUT,
            &format!("/api/issues/{id}/tags/{key}"),
            Some(&req),
        )
        .await
    }

    /// Clear a label on an issue (`DELETE /api/issues/{id}/tags/{key}`).
    pub async fn clear_issue_tag(&self, id: i64, key: &str) -> Result<IssueView> {
        self.delete(&format!("/api/issues/{id}/tags/{key}"))
            .await
            .and_then(|v| {
                serde_json::from_value(v)
                    .map_err(|e| anyhow!("decoding response from /api/issues/{id}/tags/{key}: {e}"))
            })
    }

    // -- Settings -------------------------------------------------------------

    /// Every registered setting and its effective value (`GET /api/settings`).
    pub async fn list_settings(&self) -> Result<SettingsEnvelope> {
        self.get_typed("/api/settings").await
    }

    /// Apply setting changes: a `null` value clears a key back to its default
    /// (`PATCH /api/settings`).
    pub async fn patch_settings(&self, changes: serde_json::Map<String, Value>) -> Result<Value> {
        self.patch("/api/settings", Value::Object(changes)).await
    }

    // -- Watches ------------------------------------------------------

    /// List every watch (`GET /api/watches`).
    pub async fn list_watches(&self) -> Result<Vec<WatchView>> {
        self.get_typed("/api/watches").await
    }

    /// Get one watch by id or name (`GET /api/watches/{key}`).
    pub async fn get_watch(&self, key: &str) -> Result<WatchView> {
        self.get_typed(&format!("/api/watches/{}", Self::seg(key)))
            .await
    }

    /// Register a watch (`POST /api/watches`).
    pub async fn create_watch(&self, req: &CreateWatchReq) -> Result<WatchView> {
        self.send_typed(Method::POST, "/api/watches", Some(req))
            .await
    }

    /// Patch a watch (`PATCH /api/watches/{key}`).
    pub async fn patch_watch(&self, key: &str, req: &PatchWatchReq) -> Result<WatchView> {
        self.send_typed(
            Method::PATCH,
            &format!("/api/watches/{}", Self::seg(key)),
            Some(req),
        )
        .await
    }

    /// Delete a watch (`DELETE /api/watches/{key}`).
    pub async fn delete_watch(&self, key: &str) -> Result<Value> {
        self.delete(&format!("/api/watches/{}", Self::seg(key)))
            .await
    }

    /// Fire a round now and return the raw `{run_id, outcome, summary}`
    /// (`POST /api/watches/{key}/run`).
    pub async fn run_watch(&self, key: &str, req: &RunWatchReq) -> Result<Value> {
        let body = serde_json::to_value(req)?;
        self.post(&format!("/api/watches/{}/run", Self::seg(key)), body)
            .await
    }

    // -- API tokens -------------------------------------------------------

    /// List the user-managed API tokens (`GET /api/auth/tokens`).
    pub async fn list_tokens(&self) -> Result<Vec<TokenView>> {
        self.get_typed("/api/auth/tokens").await
    }

    /// Mint a new API token, returning the one-time plaintext
    /// (`POST /api/auth/tokens`).
    pub async fn create_token(&self, req: &CreateTokenReq) -> Result<CreatedTokenView> {
        self.send_typed(Method::POST, "/api/auth/tokens", Some(req))
            .await
    }

    /// Revoke an API token by id (`DELETE /api/auth/tokens/{id}`).
    pub async fn revoke_token(&self, id: &str) -> Result<Value> {
        self.delete(&format!("/api/auth/tokens/{id}")).await
    }
}
