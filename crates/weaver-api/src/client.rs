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

use crate::operations::{
    issues as issue_operations, permissions as permission_operations, ApiMetaView, ApiOperation,
    OperationView,
};

use crate::dto::{
    AddReviewCommentReq, AnchorDto, ArtifactMeta, ArtifactUpsertReq, ArtifactView,
    AutomationTokenReq, AutomationTokenView, BranchStatusReq, BranchView, ChannelBindingView,
    ChannelMessageView, ChannelSubscriptionView, ChannelView, CloneProfileReq, CommentDto,
    CreateChannelMessageReq, CreateChannelReq, CreateEventReq, CreateIssueReq,
    CreatePermissionRequestReq, CreateRepoIssueReq, CreateReq, CreateReviewReq,
    CreateSessionGroupReq, CreateSessionSpaceReq, CreateTokenReq, CreateWatchReq, CreatedTokenView,
    CustomMcpReq, CustomMcpView, DecidePermissionRequestReq, DeleteSessionGroupReq,
    DeleteSessionSpaceReq, DeploymentReq, DeploymentView, DiagnosticsView,
    EffectivePermissionsView, EffectiveProfileView, EnsureResumptionCueReq,
    ExpectedReviewRevisionReq, FederationReq, FederationView, GithubTokenView, HandoffReq,
    HistoryPageView, IssueActionsReq, IssueActionsResult, IssueView, McpRegistryView,
    MoveSessionsReq, NewCommentBody, NewThreadBody, PatchIssueReq, PatchSessionReq, PatchWatchReq,
    PermissionRequestView, ProfileReq, ProfileView, PutProfileEnvReq, ReadinessView,
    ReorderSessionLayoutReq, ResolveLaunchReq, ResolveReviewCommentReq, ResolvedLaunchView,
    RestoreSessionGroupsReq, ResumptionCueView, ReviewCommentDto, ReviewDto, RunReq, RunView,
    RunWatchReq, ScratchLimitsView, SearchSessionsOptions, SelfContextView, SendReq,
    SessionCatchupView, SessionGithubAccessView, SessionGroupPreferenceReq, SessionLayoutView,
    SessionPlacementSelectorKind, SessionView, SetChannelReadMarkerReq, SetChannelSubscriptionReq,
    SetSessionGithubAccessReq, SetSessionPlacementDefaultReq, SetTagsReq, SetTitleGenerationReq,
    SettingsEnvelope, SubmitReviewReq, TagReq, ThreadDto, TokenView, UpdateReviewCommentReq,
    UpdateReviewReq, UpdateSessionGroupReq, UpdateSessionSpaceReq, WatchView,
};

/// A client for one loom server, identified by its base URL.
#[derive(Clone)]
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
        crate::operations::encode_path_segment(s)
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
                let sandbox_hint = std::env::var("CODEX_SANDBOX_NETWORK_DISABLED")
                    .is_ok_and(|value| value != "0");
                if sandbox_hint {
                    anyhow!(
                        "cannot reach loom at {} — the server may be unavailable (start it with `loom server start`, or check $WEAVER_API), or Codex's network sandbox may have blocked this command; run `weaver` directly (not through `sh -c`/`bash -c`) so its command rule applies",
                        self.base
                    )
                } else {
                    anyhow!(
                        "cannot reach loom at {} — no active loom session (start the server with `loom server start`, or check $WEAVER_API)",
                        self.base
                    )
                }
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

    /// Invoke one code-registered operation through Loom's canonical JSON boundary
    /// and deserialize the associated output type.
    pub async fn invoke<O: ApiOperation>(&self, input: &O::Input) -> Result<O::Output> {
        let value = serde_json::to_value(input)?;
        let path = format!(
            "/api/{}",
            O::SPEC
                .id
                .split('.')
                .map(Self::seg)
                .collect::<Vec<_>>()
                .join("/")
        );
        let value = self.invoke_value(O::SPEC.id, value).await?;
        serde_json::from_value(value)
            .map_err(|error| anyhow!("decoding response from {path}: {error}"))
    }

    /// Untyped counterpart used by generic adapters such as MCP.  The server
    /// resolves `id` in its executable registry; adapters do not keep a second
    /// callback table.
    pub async fn invoke_value(&self, id: &str, input: Value) -> Result<Value> {
        let path = format!(
            "/api/{}",
            id.split('.').map(Self::seg).collect::<Vec<_>>().join("/")
        );
        self.send(Method::POST, &path, Some(input)).await
    }

    // -- Sessions ---------------------------------------------------------

    pub async fn self_context(&self) -> Result<SelfContextView> {
        self.get_typed("/api/self").await
    }

    /// Discover the connected Loom server and its operation registry version.
    pub async fn api_meta(&self) -> Result<ApiMetaView> {
        self.get_typed("/api/meta").await
    }

    /// List the transport-neutral operation catalogue advertised by the server.
    pub async fn operations(&self) -> Result<Vec<OperationView>> {
        self.get_typed("/api/operations").await
    }

    pub async fn operation(&self, id: &str) -> Result<OperationView> {
        self.invoke::<permission_operations::Explain>(&permission_operations::ExplainInput {
            operation: id.to_string(),
        })
        .await
    }

    pub async fn github_token(&self, session_id: &str) -> Result<GithubTokenView> {
        self.send_typed::<Value, GithubTokenView>(
            Method::POST,
            &format!("/api/sessions/{}/github/token", Self::seg(session_id)),
            None,
        )
        .await
    }

    pub async fn session_github_access(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionGithubAccessView>> {
        self.get_typed(&format!(
            "/api/sessions/{}/github/access",
            Self::seg(session_id)
        ))
        .await
    }

    pub async fn set_session_github_access(
        &self,
        session_id: &str,
        request: &SetSessionGithubAccessReq,
    ) -> Result<SessionGithubAccessView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/sessions/{}/github/access", Self::seg(session_id)),
            Some(request),
        )
        .await
    }

    pub async fn effective_permissions(
        &self,
        session_id: &str,
    ) -> Result<EffectivePermissionsView> {
        self.invoke::<permission_operations::EffectiveGet>(&permission_operations::SessionInput {
            session: session_id.to_string(),
        })
        .await
    }

    pub async fn permission_requests(
        &self,
        session_id: &str,
        state: Option<&str>,
    ) -> Result<Vec<PermissionRequestView>> {
        self.invoke::<permission_operations::RequestsList>(
            &permission_operations::ListRequestsInput {
                session: session_id.to_string(),
                state: state.map(str::to_string),
            },
        )
        .await
    }

    pub async fn create_permission_request(
        &self,
        session_id: &str,
        request: &CreatePermissionRequestReq,
    ) -> Result<PermissionRequestView> {
        self.invoke::<permission_operations::RequestsCreate>(
            &permission_operations::CreateRequestInput {
                session: session_id.to_string(),
                request: request.clone(),
            },
        )
        .await
    }

    pub async fn decide_permission_request(
        &self,
        request_id: &str,
        request: &DecidePermissionRequestReq,
    ) -> Result<PermissionRequestView> {
        self.send_typed(
            Method::POST,
            &format!(
                "/api/permission-requests/{}/decision",
                Self::seg(request_id)
            ),
            Some(request),
        )
        .await
    }

    /// List active non-automation sessions (`GET /api/sessions`).
    pub async fn list_sessions(&self) -> Result<Vec<SessionView>> {
        self.get_typed("/api/sessions").await
    }

    /// Search the documented fleet facets with typed route scope and filters.
    pub async fn search_sessions(
        &self,
        options: &SearchSessionsOptions,
    ) -> Result<Vec<SessionView>> {
        let mut query = vec![
            format!("q={}", Self::seg(&options.query)),
            format!("history={}", options.history),
            format!("archived_only={}", options.archived_only),
        ];
        if let Some(status) = options.status {
            query.push(format!("status={status}"));
        }
        if let Some(attention) = options.attention {
            query.push(format!("attention={attention}"));
        }
        if let Some(creator) = options.creator {
            query.push(format!("creator={creator}"));
        }
        self.get_typed(&format!("/api/sessions/search?{}", query.join("&")))
            .await
    }

    // -- Session layout ---------------------------------------------------

    pub async fn get_session_layout(&self) -> Result<SessionLayoutView> {
        self.get_typed("/api/session-layout").await
    }

    pub async fn create_session_space(
        &self,
        req: &CreateSessionSpaceReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(Method::POST, "/api/session-layout/spaces", Some(req))
            .await
    }

    pub async fn update_session_space(
        &self,
        id: &str,
        req: &UpdateSessionSpaceReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(
            Method::PATCH,
            &format!("/api/session-layout/spaces/{}", Self::seg(id)),
            Some(req),
        )
        .await
    }

    pub async fn delete_session_space(
        &self,
        id: &str,
        req: &DeleteSessionSpaceReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(
            Method::DELETE,
            &format!("/api/session-layout/spaces/{}", Self::seg(id)),
            Some(req),
        )
        .await
    }

    pub async fn create_session_group(
        &self,
        req: &CreateSessionGroupReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(Method::POST, "/api/session-layout/groups", Some(req))
            .await
    }

    pub async fn update_session_group(
        &self,
        id: &str,
        req: &UpdateSessionGroupReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(
            Method::PATCH,
            &format!("/api/session-layout/groups/{}", Self::seg(id)),
            Some(req),
        )
        .await
    }

    pub async fn delete_session_group(
        &self,
        id: &str,
        req: &DeleteSessionGroupReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(
            Method::DELETE,
            &format!("/api/session-layout/groups/{}", Self::seg(id)),
            Some(req),
        )
        .await
    }

    pub async fn reorder_session_layout(
        &self,
        req: &ReorderSessionLayoutReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(Method::POST, "/api/session-layout/reorder", Some(req))
            .await
    }

    pub async fn move_sessions(&self, req: &MoveSessionsReq) -> Result<SessionLayoutView> {
        self.send_typed(Method::POST, "/api/session-layout/moves", Some(req))
            .await
    }

    pub async fn restore_session_groups(
        &self,
        req: &RestoreSessionGroupsReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(Method::POST, "/api/session-layout/restores", Some(req))
            .await
    }

    pub async fn set_session_group_preference(
        &self,
        id: &str,
        req: &SessionGroupPreferenceReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/session-layout/groups/{}/preference", Self::seg(id)),
            Some(req),
        )
        .await
    }

    pub async fn set_session_placement_default(
        &self,
        req: &SetSessionPlacementDefaultReq,
    ) -> Result<SessionLayoutView> {
        self.send_typed(Method::PUT, "/api/session-layout/defaults", Some(req))
            .await
    }

    pub async fn delete_session_placement_default(
        &self,
        kind: SessionPlacementSelectorKind,
        value: &str,
        expected_revision: i64,
    ) -> Result<SessionLayoutView> {
        self.send_typed::<(), SessionLayoutView>(
            Method::DELETE,
            &format!(
                "/api/session-layout/defaults/{}/{}?expected_revision={expected_revision}",
                kind,
                Self::seg(value)
            ),
            None,
        )
        .await
    }

    /// Get one session by key — id, branch id, branch name, or `repo:branch`
    /// (`GET /api/sessions/{key}`).
    pub async fn get_session(&self, key: &str) -> Result<SessionView> {
        self.get_typed(&format!("/api/sessions/{}", Self::seg(key)))
            .await
    }

    pub async fn session_summary(&self, key: &str) -> Result<SessionCatchupView> {
        self.get_typed(&format!("/api/sessions/{}/summary", Self::seg(key)))
            .await
    }

    /// Page normalized records for one session
    /// (`GET /api/sessions/{key}/history`). `before` is the exclusive opaque
    /// cursor returned by the preceding page.
    pub async fn get_session_history(
        &self,
        key: &str,
        before: Option<&str>,
        limit: Option<usize>,
        kinds: &[String],
    ) -> Result<HistoryPageView> {
        let query = Self::history_query(before, limit, kinds, None);
        self.get_typed(&format!("/api/sessions/{}/history{query}", Self::seg(key)))
            .await
    }

    /// Case-insensitive literal search over one session's normalized records
    /// (`GET /api/sessions/{key}/history/search`).
    pub async fn search_session_history(
        &self,
        key: &str,
        search: &str,
        before: Option<&str>,
        limit: Option<usize>,
        kinds: &[String],
    ) -> Result<HistoryPageView> {
        let query = Self::history_query(before, limit, kinds, Some(search));
        self.get_typed(&format!(
            "/api/sessions/{}/history/search{query}",
            Self::seg(key)
        ))
        .await
    }

    fn history_query(
        before: Option<&str>,
        limit: Option<usize>,
        kinds: &[String],
        search: Option<&str>,
    ) -> String {
        let encode = |value: &str| {
            percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
                .to_string()
        };
        let mut fields = Vec::new();
        if let Some(search) = search {
            fields.push(format!("q={}", encode(search)));
        }
        if let Some(before) = before {
            fields.push(format!("before={}", encode(before)));
        }
        if let Some(limit) = limit {
            fields.push(format!("limit={limit}"));
        }
        if !kinds.is_empty() {
            fields.push(format!("kinds={}", encode(&kinds.join(","))));
        }
        if fields.is_empty() {
            String::new()
        } else {
            format!("?{}", fields.join("&"))
        }
    }

    /// Launch a new session (`POST /api/sessions`).
    pub async fn create_session(&self, req: &CreateReq) -> Result<SessionView> {
        self.send_typed(Method::POST, "/api/sessions", Some(req))
            .await
    }

    /// Resolve and validate a profile selection without launching.
    pub async fn resolve_session_launch(
        &self,
        req: &ResolveLaunchReq,
    ) -> Result<ResolvedLaunchView> {
        self.send_typed(Method::POST, "/api/session-launches/resolve", Some(req))
            .await
    }

    /// Resolve a canonical profile selection in the context of an existing
    /// session handoff, including honest class and capacity credit.
    pub async fn resolve_session_handoff(
        &self,
        key: &str,
        req: &ResolveLaunchReq,
    ) -> Result<ResolvedLaunchView> {
        self.send_typed(
            Method::POST,
            &format!("/api/sessions/{}/handoff/resolve", Self::seg(key)),
            Some(req),
        )
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

    pub async fn regenerate_session_title(&self, key: &str) -> Result<SessionView> {
        self.send_typed::<(), _>(
            Method::POST,
            &format!("/api/sessions/{}/title/regenerate", Self::seg(key)),
            None,
        )
        .await
    }

    pub async fn set_session_title_generation(
        &self,
        key: &str,
        req: &SetTitleGenerationReq,
    ) -> Result<SessionView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/sessions/{}/title-generation", Self::seg(key)),
            Some(req),
        )
        .await
    }

    pub async fn get_resumption_cue(&self, key: &str) -> Result<ResumptionCueView> {
        self.send_typed::<(), _>(
            Method::GET,
            &format!("/api/sessions/{}/resumption-cue", Self::seg(key)),
            None,
        )
        .await
    }

    pub async fn ensure_resumption_cue(
        &self,
        key: &str,
        req: &EnsureResumptionCueReq,
    ) -> Result<ResumptionCueView> {
        self.send_typed(
            Method::POST,
            &format!("/api/sessions/{}/resumption-cue", Self::seg(key)),
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

    /// Atomically replace every tag authored by `by` on a session. Exact
    /// `(key, value)` entries in `clear` are removed in the same transaction.
    pub async fn set_tags(&self, key: &str, req: &SetTagsReq) -> Result<SessionView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/sessions/{}/tags", Self::seg(key)),
            Some(req),
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
    /// (`POST /api/sessions/{key}/send`). For ACP, this steers a supported live
    /// turn and otherwise cancels it before starting a normal turn.
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

    /// Typed, bounded worktree changes relative to the session's local base.
    pub async fn changes(&self, key: &str) -> Result<crate::ChangeSetDto> {
        self.get_typed(&format!("/api/sessions/{}/changes", Self::seg(key)))
            .await
    }

    /// Recent events for a branch, newest first, capped at 200 server-side
    /// (`GET /api/branches/{key}/events`). `key` may be a branch id,
    /// `repo:branch`, or unambiguous id prefix — no live session required.
    pub async fn branch_log(&self, key: &str) -> Result<Vec<weaver_core::events::Event>> {
        self.get_typed(&format!("/api/branches/{}/events", Self::seg(key)))
            .await
    }

    // -- Channels ----------------------------------------------------------

    pub async fn list_channels(&self, archived: bool) -> Result<Vec<ChannelView>> {
        self.get_typed(&format!("/api/channels?archived={archived}"))
            .await
    }

    pub async fn create_channel(&self, req: &CreateChannelReq) -> Result<ChannelView> {
        self.send_typed(Method::POST, "/api/channels", Some(req))
            .await
    }

    pub async fn get_channel(&self, id: &str) -> Result<ChannelView> {
        self.get_typed(&format!("/api/channels/{}", Self::seg(id)))
            .await
    }

    pub async fn channel_bindings(&self, id: &str) -> Result<Vec<ChannelBindingView>> {
        self.get_typed(&format!("/api/channels/{}/bindings", Self::seg(id)))
            .await
    }

    pub async fn channel_messages(&self, id: &str, after: i64) -> Result<Vec<ChannelMessageView>> {
        self.get_typed(&format!(
            "/api/channels/{}/messages?after={}",
            Self::seg(id),
            after.max(0)
        ))
        .await
    }

    pub async fn channel_messages_bounded(
        &self,
        id: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ChannelMessageView>> {
        self.get_typed(&format!(
            "/api/channels/{}/messages?after={}&limit={limit}",
            Self::seg(id),
            after.max(0)
        ))
        .await
    }

    pub async fn send_channel_message(
        &self,
        id: &str,
        req: &CreateChannelMessageReq,
    ) -> Result<ChannelMessageView> {
        self.send_typed(
            Method::POST,
            &format!("/api/channels/{}/messages", Self::seg(id)),
            Some(req),
        )
        .await
    }

    pub async fn set_channel_subscription(
        &self,
        id: &str,
        mode: &str,
        session_id: Option<&str>,
    ) -> Result<ChannelSubscriptionView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/channels/{}/subscription", Self::seg(id)),
            Some(&SetChannelSubscriptionReq {
                mode: mode.to_string(),
                session_id: session_id.map(str::to_string),
            }),
        )
        .await
    }

    pub async fn mark_channel_read(
        &self,
        id: &str,
        seq: Option<i64>,
    ) -> Result<ChannelSubscriptionView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/channels/{}/read-marker", Self::seg(id)),
            Some(&SetChannelReadMarkerReq { seq }),
        )
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
    // against the branch row directly: what the `loom artifacts` CLI needs,
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
    // The twin of Loom's session-scoped thread routes, for `loom artifacts
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

    // -- Staged reviews ------------------------------------------------------

    pub async fn list_session_reviews(
        &self,
        session: &str,
        subject_kind: &str,
        subject_key: &str,
    ) -> Result<Vec<ReviewDto>> {
        self.get_typed(&format!(
            "/api/sessions/{}/reviews?subject_kind={}&subject_key={}",
            Self::seg(session),
            Self::seg(subject_kind),
            Self::seg(subject_key)
        ))
        .await
    }

    pub async fn create_session_review(
        &self,
        session: &str,
        req: &CreateReviewReq,
    ) -> Result<ReviewDto> {
        self.send_typed(
            Method::POST,
            &format!("/api/sessions/{}/reviews", Self::seg(session)),
            Some(req),
        )
        .await
    }

    pub async fn add_review_comment(
        &self,
        review_id: i64,
        req: &AddReviewCommentReq,
    ) -> Result<ReviewDto> {
        self.send_typed(
            Method::POST,
            &format!("/api/reviews/{review_id}/comments"),
            Some(req),
        )
        .await
    }

    pub async fn get_review(&self, review_id: i64) -> Result<ReviewDto> {
        self.get_typed(&format!("/api/reviews/{review_id}")).await
    }

    pub async fn update_review_comment(
        &self,
        review_id: i64,
        comment_id: i64,
        req: &UpdateReviewCommentReq,
    ) -> Result<ReviewDto> {
        self.send_typed(
            Method::PATCH,
            &format!("/api/reviews/{review_id}/comments/{comment_id}"),
            Some(req),
        )
        .await
    }

    pub async fn update_review(&self, review_id: i64, req: &UpdateReviewReq) -> Result<ReviewDto> {
        self.send_typed(
            Method::PATCH,
            &format!("/api/reviews/{review_id}"),
            Some(req),
        )
        .await
    }

    pub async fn delete_review_comment(
        &self,
        review_id: i64,
        comment_id: i64,
        expected_revision: i64,
    ) -> Result<ReviewDto> {
        self.send_typed(
            Method::DELETE,
            &format!("/api/reviews/{review_id}/comments/{comment_id}"),
            Some(&ExpectedReviewRevisionReq { expected_revision }),
        )
        .await
    }

    pub async fn discard_review(&self, review_id: i64, expected_revision: i64) -> Result<Value> {
        self.send_typed(
            Method::DELETE,
            &format!("/api/reviews/{review_id}"),
            Some(&ExpectedReviewRevisionReq { expected_revision }),
        )
        .await
    }

    pub async fn submit_review(&self, review_id: i64, req: &SubmitReviewReq) -> Result<ReviewDto> {
        self.send_typed(
            Method::POST,
            &format!("/api/reviews/{review_id}/submit"),
            Some(req),
        )
        .await
    }

    pub async fn retarget_review_to_current(
        &self,
        review_id: i64,
        expected_revision: i64,
    ) -> Result<ReviewDto> {
        self.send_typed(
            Method::POST,
            &format!("/api/reviews/{review_id}/retarget-current"),
            Some(&ExpectedReviewRevisionReq { expected_revision }),
        )
        .await
    }

    pub async fn set_review_comment_resolution(
        &self,
        review_id: i64,
        comment_id: i64,
        resolved: bool,
    ) -> Result<ReviewCommentDto> {
        self.send_typed(
            Method::POST,
            &format!("/api/reviews/{review_id}/comments/{comment_id}/resolve"),
            Some(&ResolveReviewCommentReq { resolved }),
        )
        .await
    }

    /// Compatibility alias for [`Self::set_review_comment_resolution`].
    pub async fn resolve_review_comment(
        &self,
        review_id: i64,
        comment_id: i64,
        resolved: bool,
    ) -> Result<ReviewCommentDto> {
        self.set_review_comment_resolution(review_id, comment_id, resolved)
            .await
    }

    pub async fn retry_review_delivery(&self, review_id: i64) -> Result<ReviewDto> {
        self.send_typed::<Value, ReviewDto>(
            Method::POST,
            &format!("/api/reviews/{review_id}/retry-delivery"),
            Some(&Value::Object(Default::default())),
        )
        .await
    }

    // -- Issues ---------------------------------------------------------------

    /// Create an issue claimed by a branch (`POST /api/issues/create`).
    pub async fn create_branch_issue(&self, key: &str, req: &CreateIssueReq) -> Result<IssueView> {
        self.invoke::<issue_operations::Create>(&issue_operations::CreateInput {
            branch: key.to_string(),
            request: req.clone(),
        })
        .await
    }

    /// Create an unclaimed repo-level backlog item
    /// (`POST /api/issues/backlog/create`).
    pub async fn create_repo_issue(&self, req: &CreateRepoIssueReq) -> Result<IssueView> {
        self.invoke::<issue_operations::CreateBacklog>(req).await
    }

    /// Every issue in a repo (`scope: "repo"`), or just the unclaimed backlog
    /// (`scope: "backlog"`) — the one fetch every `loom issues list` view
    /// partitions client-side (`POST /api/issues/list`).
    pub async fn list_repo_issues(
        &self,
        repo_root: &str,
        scope: &str,
        all: bool,
    ) -> Result<Vec<IssueView>> {
        self.invoke::<issue_operations::List>(&issue_operations::ListInput {
            repo_root: repo_root.to_string(),
            scope: scope.parse().map_err(anyhow::Error::msg)?,
            all,
        })
        .await
    }

    /// Get one issue by id (`POST /api/issues/get`).
    pub async fn get_issue(&self, id: i64) -> Result<IssueView> {
        self.invoke::<issue_operations::Get>(&issue_operations::IdInput { id })
            .await
    }

    /// Patch an issue's title/body/status (`PATCH /api/issues/{id}`).
    pub async fn patch_issue(&self, id: i64, req: &PatchIssueReq) -> Result<IssueView> {
        self.send_typed(Method::PATCH, &format!("/api/issues/{id}"), Some(req))
            .await
    }

    /// Apply one command to a set of issues (`POST /api/issues/actions`).
    ///
    /// The server validates every id and precondition before applying the
    /// command atomically. Validation failure changes nothing.
    pub async fn issue_actions(&self, req: &IssueActionsReq) -> Result<IssueActionsResult> {
        self.invoke::<issue_operations::Actions>(req).await
    }

    /// Close one issue through its semantic operation.
    pub async fn close_issue(&self, id: i64) -> Result<IssueView> {
        self.invoke::<issue_operations::Close>(&issue_operations::IdInput { id })
            .await
    }

    /// Reopen one issue through its semantic operation.
    pub async fn reopen_issue(&self, id: i64) -> Result<IssueView> {
        self.invoke::<issue_operations::Reopen>(&issue_operations::IdInput { id })
            .await
    }

    /// Delete an issue (`POST /api/issues/delete`).
    pub async fn delete_issue(&self, id: i64) -> Result<Value> {
        let deleted = self
            .invoke::<issue_operations::Delete>(&issue_operations::IdInput { id })
            .await?;
        Ok(serde_json::to_value(deleted)?)
    }

    /// Set (upsert) a free-form label on an issue
    /// (`POST /api/issues/tags/set`). Issue tags carry no
    /// `attention`/`triage` ladder — every key is a quiet annotation.
    pub async fn set_issue_tag(
        &self,
        id: i64,
        key: &str,
        value: &str,
        note: &str,
        by: &str,
    ) -> Result<IssueView> {
        self.invoke::<issue_operations::SetTag>(&issue_operations::SetTagInput {
            id,
            key: key.to_string(),
            request: TagReq {
                value: value.to_string(),
                note: note.to_string(),
                by: Some(by.to_string()),
            },
        })
        .await
    }

    /// Clear a label on an issue (`POST /api/issues/tags/delete`).
    pub async fn clear_issue_tag(&self, id: i64, key: &str) -> Result<IssueView> {
        self.invoke::<issue_operations::DeleteTag>(&issue_operations::DeleteTagInput {
            id,
            key: key.to_string(),
        })
        .await
    }

    // -- Settings -------------------------------------------------------------

    /// Public database and migration readiness (`GET /api/ready`).
    pub async fn readiness(&self) -> Result<ReadinessView> {
        self.get_typed("/api/ready").await
    }

    /// Human-readable redacted operational inventory (`GET /api/diagnostics`).
    pub async fn diagnostics(&self) -> Result<DiagnosticsView> {
        self.get_typed("/api/diagnostics").await
    }

    /// Trusted MCP adapters and their provider-neutral capability sets.
    pub async fn mcp_registry(&self) -> Result<McpRegistryView> {
        self.get_typed("/api/mcps").await
    }

    pub async fn create_custom_mcp(&self, req: &CustomMcpReq) -> Result<CustomMcpView> {
        self.send_typed(Method::POST, "/api/mcps/custom", Some(req))
            .await
    }

    pub async fn put_custom_mcp(
        &self,
        identity: &str,
        req: &CustomMcpReq,
    ) -> Result<CustomMcpView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/mcps/custom/{}", identity.trim_start_matches('/')),
            Some(req),
        )
        .await
    }

    pub async fn delete_custom_mcp(&self, identity: &str) -> Result<Value> {
        self.delete(&format!(
            "/api/mcps/custom/{}",
            identity.trim_start_matches('/')
        ))
        .await
    }

    /// List named launch profiles. Secret environment values are withheld.
    pub async fn list_profiles(&self) -> Result<Vec<ProfileView>> {
        self.get_typed("/api/profiles").await
    }

    pub async fn get_profile(&self, name: &str) -> Result<ProfileView> {
        self.get_typed(&format!("/api/profiles/{}", Self::seg(name)))
            .await
    }

    pub async fn effective_profile(&self, name: &str) -> Result<EffectiveProfileView> {
        self.get_typed(&format!("/api/profiles/{}/effective", Self::seg(name)))
            .await
    }

    pub async fn create_profile(&self, req: &ProfileReq) -> Result<ProfileView> {
        self.send_typed(Method::POST, "/api/profiles", Some(req))
            .await
    }

    pub async fn put_profile(&self, name: &str, req: &ProfileReq) -> Result<ProfileView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/profiles/{}", Self::seg(name)),
            Some(req),
        )
        .await
    }

    /// Clone one profile's policy on the server, optionally copying its
    /// write-only environment in the same transaction.
    pub async fn clone_profile(&self, source: &str, req: &CloneProfileReq) -> Result<ProfileView> {
        self.send_typed(
            Method::POST,
            &format!("/api/profiles/{}/clone", Self::seg(source)),
            Some(req),
        )
        .await
    }

    /// Return the upload limits shared by launch and live-session Scratch.
    pub async fn scratch_limits(&self) -> Result<ScratchLimitsView> {
        self.get_typed("/api/scratch/limits").await
    }

    pub async fn delete_profile(&self, name: &str) -> Result<Value> {
        self.delete(&format!("/api/profiles/{}", Self::seg(name)))
            .await
    }

    pub async fn set_profile_env(
        &self,
        profile: &str,
        name: &str,
        value: &str,
    ) -> Result<ProfileView> {
        let req = PutProfileEnvReq {
            value: Some(value.to_string()),
            secret_ref: None,
        };
        self.send_typed(
            Method::PUT,
            &format!(
                "/api/profiles/{}/env/{}",
                Self::seg(profile),
                Self::seg(name)
            ),
            Some(&req),
        )
        .await
    }

    pub async fn set_profile_env_secret(
        &self,
        profile: &str,
        name: &str,
        secret_ref: &str,
    ) -> Result<ProfileView> {
        let req = PutProfileEnvReq {
            value: None,
            secret_ref: Some(secret_ref.to_string()),
        };
        self.send_typed(
            Method::PUT,
            &format!(
                "/api/profiles/{}/env/{}",
                Self::seg(profile),
                Self::seg(name)
            ),
            Some(&req),
        )
        .await
    }

    pub async fn remove_profile_env(&self, profile: &str, name: &str) -> Result<ProfileView> {
        let value = self
            .delete(&format!(
                "/api/profiles/{}/env/{}",
                Self::seg(profile),
                Self::seg(name)
            ))
            .await?;
        serde_json::from_value(value).map_err(|e| anyhow!("decoding profile response: {e}"))
    }

    /// Every registered setting and its effective value (`GET /api/settings`).
    pub async fn list_settings(&self) -> Result<SettingsEnvelope> {
        self.get_typed("/api/settings").await
    }

    /// Apply setting changes: a `null` value clears a key back to its default
    /// (`PATCH /api/settings`).
    pub async fn patch_settings(
        &self,
        changes: serde_json::Map<String, Value>,
    ) -> Result<SettingsEnvelope> {
        self.send_typed(Method::PATCH, "/api/settings", Some(&changes))
            .await
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

    pub async fn mint_automation_token(
        &self,
        req: &AutomationTokenReq,
    ) -> Result<AutomationTokenView> {
        self.send_typed(Method::POST, "/api/auth/automation-token", Some(req))
            .await
    }

    pub async fn list_federations(&self) -> Result<Vec<FederationView>> {
        self.get_typed("/api/auth/federations").await
    }

    pub async fn add_federation(&self, req: &FederationReq) -> Result<FederationView> {
        self.send_typed(Method::POST, "/api/auth/federations", Some(req))
            .await
    }

    pub async fn remove_federation(&self, id: &str) -> Result<Value> {
        self.delete(&format!("/api/auth/federations/{}", Self::seg(id)))
            .await
    }

    pub async fn reconcile_deployment(&self, req: &DeploymentReq) -> Result<DeploymentView> {
        self.send_typed(Method::POST, "/api/deployment/reconcile", Some(req))
            .await
    }

    pub async fn create_run(&self, req: &RunReq) -> Result<RunView> {
        self.send_typed(Method::POST, "/api/runs", Some(req)).await
    }

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
