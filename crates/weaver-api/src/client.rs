//! The typed loom REST client. A thin layer over an untyped JSON `send`: the
//! untyped `get`/`post`/`patch`/`delete` are kept for callers that pretty-print
//! raw JSON (the `loom` CLI), and the typed methods over them each invoke one
//! code-registered operation, serializing its `Input` and deserializing its
//! `Output` — the surface the Python binding wraps.

use anyhow::{anyhow, bail, Result};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::operations::{ApiMetaView, Operation, OperationView};

use crate::dto::{
    AddReviewCommentReq, AnchorDto, ArtifactMeta, ArtifactUpsertReq, ArtifactView,
    AutomationTokenReq, AutomationTokenView, BranchView, ChannelBindingView, ChannelMessageView,
    ChannelSubscriptionView, ChannelView, CloneProfileReq, CommentDto, CreateChannelMessageReq,
    CreateChannelReq, CreateReq, CreateReviewReq, CreateSessionGroupReq, CreateSessionSpaceReq,
    CreateTokenReq, CreateWatchReq, CreatedTokenView, CustomMcpReq, CustomMcpView,
    DecidePermissionRequestReq, DeleteSessionGroupReq, DeleteSessionSpaceReq, DeploymentReq,
    DeploymentView, DiagnosticsView, EffectiveProfileView, EnsureResumptionCueReq, FederationReq,
    FederationView, GithubTokenView, HandoffReq, HistoryPageView, IssueView, McpRegistryView,
    MoveSessionsReq, PatchIssueReq, PatchSessionReq, PatchWatchReq, PermissionRequestView,
    ProfileReq, ProfileView, ReadinessView, ReorderSessionLayoutReq, ResolveLaunchReq,
    ResolvedLaunchView, RestoreSessionGroupsReq, ResumptionCueView, ReviewCommentDto, ReviewDto,
    ReviewSubjectKindDto, RunReq, RunView, RunWatchReq, ScratchLimitsView, SearchSessionsOptions,
    SelfContextView, SendReq, SessionCatchupView, SessionGithubAccessView,
    SessionGroupPreferenceReq, SessionLayoutView, SessionPlacementSelectorKind, SessionView,
    SetSessionGithubAccessReq, SetSessionPlacementDefaultReq, SetTagsReq, SetTitleGenerationReq,
    SettingsEnvelope, SubmitReviewReq, ThreadDto, TokenView, UpdateReviewCommentReq,
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

    /// Percent-encode a value embedded as a single URL path segment. Needed
    /// only by [`Client::set_tags`], the one method left on a hand-written
    /// route: a session key can be `repo_root:branch`, and a real repo root is
    /// an absolute path full of `/`, which would otherwise split into extra
    /// path segments the router never matches.
    fn seg(s: &str) -> String {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    }

    // -- Untyped JSON transport -------------------------------------------

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let method_name = method.to_string();
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
            // Name the request. A bare "server returned 405" says nothing about
            // which of a hundred operations was refused, and 404/405 in
            // particular are almost always about the path rather than the body.
            bail!(
                "server returned {} for {} {path} — {}",
                status.as_u16(),
                method_name,
                message
            );
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
    pub async fn invoke<O: Operation>(&self, input: &O::Input) -> Result<O::Output> {
        let value = serde_json::to_value(input)?;
        let path = O::SPEC.path();
        let value = self.invoke_value(O::SPEC.id, value).await?;
        serde_json::from_value(value)
            .map_err(|error| anyhow!("decoding response from {path}: {error}"))
    }

    /// Untyped counterpart used by generic adapters such as MCP.  The server
    /// resolves `id` in its executable registry; adapters do not keep a second
    /// callback table.
    pub async fn invoke_value(&self, id: &str, input: Value) -> Result<Value> {
        self.send(Method::POST, &Self::operation_path(id), Some(input))
            .await
    }

    /// An operation id's route, derived exactly as `OperationSpec::path` derives
    /// it.
    ///
    /// Not percent-encoded, and that is the point: this used to run each dotted
    /// segment through [`Self::seg`], which escapes `NON_ALPHANUMERIC` and so
    /// turned every id containing an underscore into a path the server does not
    /// serve — `channels.read_marker.set` was requested as
    /// `/api/channels/read%5Fmarker/set` and came back 405. An operation id
    /// cannot contain a slash: it is `[a-z0-9_]` joined by dots, which is why
    /// the derivation is a plain substitution on both sides.
    fn operation_path(id: &str) -> String {
        format!("/api/{}", id.replace('.', "/"))
    }

    // -- Sessions ---------------------------------------------------------

    pub async fn self_context(&self) -> Result<SelfContextView> {
        use crate::operations::sessions::context;
        self.invoke::<context::Get>(&context::Input {
            session: String::new(),
        })
        .await
    }

    /// Discover the connected Loom server and its operation registry version.
    pub async fn api_meta(&self) -> Result<ApiMetaView> {
        self.get_typed("/api/meta").await
    }

    /// List the transport-neutral operation catalogue advertised by the server.
    pub async fn operations(&self) -> Result<Vec<OperationView>> {
        self.get_typed("/api/operations").await
    }

    /// Broker a short-lived GitHub App installation token scoped to a
    /// session's effective repositories (`permissions.github.token`).
    pub async fn github_token(&self, session_id: &str) -> Result<GithubTokenView> {
        use crate::operations::permissions::github::token;
        self.invoke::<token::Token>(&token::Input {
            session: session_id.to_string(),
        })
        .await
    }

    /// The repository access one session has been granted
    /// (`sessions.github.access.list`).
    pub async fn session_github_access(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionGithubAccessView>> {
        use crate::operations::sessions::github::access::list;
        self.invoke::<list::List>(&list::Input {
            session: session_id.to_string(),
        })
        .await
    }

    /// Grant or revoke one session's override access to a GitHub repository.
    /// Dispatches to `permissions.github.grant` for `mode: "write"`, else
    /// `permissions.github.revoke` — the two operations that replaced this
    /// route's `PUT` half; `write`/`none` are the only modes the store
    /// recognizes.
    pub async fn set_session_github_access(
        &self,
        session_id: &str,
        request: &SetSessionGithubAccessReq,
    ) -> Result<SessionGithubAccessView> {
        use crate::operations::permissions::github::{grant, revoke};
        if request.mode == "write" {
            self.invoke::<grant::Grant>(&grant::Input {
                repository: request.repository.clone(),
                session: session_id.to_string(),
            })
            .await
        } else {
            self.invoke::<revoke::Revoke>(&revoke::Input {
                repository: request.repository.clone(),
                session: session_id.to_string(),
            })
            .await
        }
    }

    /// Approve or deny a pending permission request.
    ///
    /// The decision used to be a field in the body of one route; it is now the
    /// choice of operation, which is what lets `permissions.requests.approve`
    /// carry `risk = ExternalWrite` while `deny` is an ordinary write.
    pub async fn decide_permission_request(
        &self,
        request_id: &str,
        request: &DecidePermissionRequestReq,
    ) -> Result<PermissionRequestView> {
        use crate::operations::permissions::requests::{approve, deny};
        match request.decision.trim() {
            "approve" => {
                self.invoke::<approve::Approve>(&approve::Input {
                    request: request_id.to_string(),
                    reason: request.reason.clone(),
                })
                .await
            }
            "deny" => {
                self.invoke::<deny::Deny>(&deny::Input {
                    request: request_id.to_string(),
                    reason: request.reason.clone(),
                })
                .await
            }
            other => Err(anyhow!("unknown permission decision `{other}`")),
        }
    }

    /// List visible sessions with default filters (`sessions.list`).
    pub async fn list_sessions(&self) -> Result<Vec<SessionView>> {
        self.search_sessions(&SearchSessionsOptions::default())
            .await
    }

    /// Search the documented fleet facets (`sessions.list`).
    pub async fn search_sessions(
        &self,
        options: &SearchSessionsOptions,
    ) -> Result<Vec<SessionView>> {
        use crate::operations::sessions::list;
        self.invoke::<list::List>(&list::Input {
            q: options.query.clone(),
            history: options.history,
            archived_only: options.archived_only,
            status: options.status,
            attention: options.attention,
            creator: options.creator,
            automation: options.automation.unwrap_or(true),
            managed: options.managed,
        })
        .await
    }

    // -- Session layout ---------------------------------------------------

    pub async fn get_session_layout(&self) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::get;
        self.invoke::<get::Get>(&get::Input {}).await
    }

    pub async fn create_session_space(
        &self,
        req: &CreateSessionSpaceReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::spaces::create;
        self.invoke::<create::Create>(&create::Input {
            name: req.name.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn update_session_space(
        &self,
        id: &str,
        req: &UpdateSessionSpaceReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::spaces::update;
        self.invoke::<update::Update>(&update::Input {
            id: id.to_string(),
            name: req.name.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn delete_session_space(
        &self,
        id: &str,
        req: &DeleteSessionSpaceReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::spaces::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            id: id.to_string(),
            destination_group_id: req.destination_group_id.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn create_session_group(
        &self,
        req: &CreateSessionGroupReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::groups::create;
        self.invoke::<create::Create>(&create::Input {
            space_id: req.space_id.clone(),
            name: req.name.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn update_session_group(
        &self,
        id: &str,
        req: &UpdateSessionGroupReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::groups::update;
        self.invoke::<update::Update>(&update::Input {
            id: id.to_string(),
            name: req.name.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn delete_session_group(
        &self,
        id: &str,
        req: &DeleteSessionGroupReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::groups::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            id: id.to_string(),
            destination_group_id: req.destination_group_id.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn reorder_session_layout(
        &self,
        req: &ReorderSessionLayoutReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::reorder;
        self.invoke::<reorder::Reorder>(&reorder::Input {
            kind: req.kind,
            id: req.id.clone(),
            before_id: req.before_id.clone(),
            destination_space_id: req.destination_space_id.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn move_sessions(&self, req: &MoveSessionsReq) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::r#move;
        self.invoke::<r#move::Move>(&r#move::Input {
            session_ids: req.session_ids.clone(),
            destination_group_id: req.destination_group_id.clone(),
            before_session_id: req.before_session_id.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn restore_session_groups(
        &self,
        req: &RestoreSessionGroupsReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::restore;
        self.invoke::<restore::Restore>(&restore::Input {
            groups: req.groups.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn set_session_group_preference(
        &self,
        id: &str,
        req: &SessionGroupPreferenceReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::groups::preference::set;
        self.invoke::<set::Set>(&set::Input {
            id: id.to_string(),
            collapsed: req.collapsed,
        })
        .await
    }

    pub async fn set_session_placement_default(
        &self,
        req: &SetSessionPlacementDefaultReq,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::defaults::set;
        self.invoke::<set::Set>(&set::Input {
            selector_kind: req.selector_kind,
            selector_value: req.selector_value.clone(),
            group_id: req.group_id.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    pub async fn delete_session_placement_default(
        &self,
        kind: SessionPlacementSelectorKind,
        value: &str,
        expected_revision: i64,
    ) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::defaults::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            selector_kind: kind,
            selector_value: value.to_string(),
            expected_revision,
        })
        .await
    }

    /// Get one session by key — id, branch id, branch name, or `repo:branch`
    /// (`sessions.get`).
    pub async fn get_session(&self, key: &str) -> Result<SessionView> {
        use crate::operations::sessions::get;
        self.invoke::<get::Get>(&get::Input {
            session: key.to_string(),
        })
        .await
    }

    /// Catch-up summary for one session (`sessions.summary.get`).
    pub async fn session_summary(&self, key: &str) -> Result<SessionCatchupView> {
        use crate::operations::sessions::summary::get;
        self.invoke::<get::Get>(&get::Input {
            session: key.to_string(),
        })
        .await
    }

    /// Page normalized records for one session (`sessions.history.list`).
    /// `before` is the exclusive opaque cursor returned by the preceding page.
    pub async fn get_session_history(
        &self,
        key: &str,
        before: Option<&str>,
        limit: Option<usize>,
        kinds: &[String],
    ) -> Result<HistoryPageView> {
        use crate::operations::sessions::history::list;
        self.invoke::<list::List>(&list::Input {
            before: before.map(str::to_string),
            limit: limit.map(|l| l as i64),
            kinds: kinds.to_vec(),
            session: key.to_string(),
        })
        .await
    }

    /// Case-insensitive literal search over one session's normalized records
    /// (`sessions.history.search`).
    pub async fn search_session_history(
        &self,
        key: &str,
        search: &str,
        before: Option<&str>,
        limit: Option<usize>,
        kinds: &[String],
    ) -> Result<HistoryPageView> {
        use crate::operations::sessions::history::search;
        self.invoke::<search::Search>(&search::Input {
            q: search.to_string(),
            before: before.map(str::to_string),
            limit: limit.map(|l| l as i64),
            kinds: kinds.to_vec(),
            session: key.to_string(),
        })
        .await
    }

    /// Launch a new session (`sessions.launch`).
    pub async fn create_session(&self, req: &CreateReq) -> Result<SessionView> {
        use crate::operations::sessions::launch;
        self.invoke::<launch::Launch>(&launch::Input {
            title: req.title.clone(),
            goal: req.goal.clone(),
            repo: req.repo.clone(),
            cwd: req.cwd.clone(),
            base: req.base.clone(),
            agent: req.agent.clone(),
            protocol: req.protocol.clone(),
            mode: req.mode.clone(),
            class: req.class.clone(),
            profile: req.profile.clone(),
            claim_issue: req.claim_issue,
            issue: req.issue,
            parent_branch: req.parent_branch.clone(),
            name: req.name.clone(),
            existing_branch: req.existing_branch.clone(),
            github_issue: req.github_issue,
            model: req.model.clone(),
            effort: req.effort.clone(),
            selection: req.selection.clone(),
            scratch: req.scratch.clone(),
            expected_profile_revision: req.expected_profile_revision,
            expected_resolver_revision: req.expected_resolver_revision.clone(),
        })
        .await
    }

    /// Resolve and validate a profile selection without launching
    /// (`sessions.launches.resolve`).
    pub async fn resolve_session_launch(
        &self,
        req: &ResolveLaunchReq,
    ) -> Result<ResolvedLaunchView> {
        use crate::operations::sessions::launches::resolve;
        self.invoke::<resolve::Resolve>(&resolve::Input {
            selection: req.selection.clone(),
        })
        .await
    }

    /// Resolve a canonical profile selection in the context of an existing
    /// session handoff, including honest class and capacity credit
    /// (`sessions.handoff.resolve`).
    pub async fn resolve_session_handoff(
        &self,
        key: &str,
        req: &ResolveLaunchReq,
    ) -> Result<ResolvedLaunchView> {
        use crate::operations::sessions::handoff::resolve;
        self.invoke::<resolve::Resolve>(&resolve::Input {
            selection: req.selection.clone(),
            session: key.to_string(),
        })
        .await
    }

    /// Patch a session's lifecycle / branch fields (`sessions.update`).
    ///
    /// `PatchSessionReq`'s `park`/`sort_order` are dropped: they existed only so
    /// the legacy route could reject a layout client that had not moved to the
    /// revisioned session-layout operations, and `sessions.update` has no
    /// operand for either.
    pub async fn patch_session(&self, key: &str, req: &PatchSessionReq) -> Result<SessionView> {
        use crate::operations::sessions::update;
        self.invoke::<update::Update>(&update::Input {
            status: req.status.clone(),
            title: req.title.clone(),
            expected_title: req.expected_title.clone(),
            expected_title_provenance: req.expected_title_provenance.clone(),
            goal: req.goal.clone(),
            description: req.description.clone(),
            session: key.to_string(),
        })
        .await
    }

    /// Regenerate a session's title now, bypassing the confidence guard
    /// (`sessions.title.regenerate`).
    pub async fn regenerate_session_title(&self, key: &str) -> Result<SessionView> {
        use crate::operations::sessions::title::regenerate;
        self.invoke::<regenerate::Regenerate>(&regenerate::Input {
            session: key.to_string(),
        })
        .await
    }

    /// Toggle automatic title generation (`sessions.title.generation.set`).
    pub async fn set_session_title_generation(
        &self,
        key: &str,
        req: &SetTitleGenerationReq,
    ) -> Result<SessionView> {
        use crate::operations::sessions::title::generation::set;
        self.invoke::<set::Set>(&set::Input {
            enabled: req.enabled,
            session: key.to_string(),
        })
        .await
    }

    /// The session's current resumption cue (`sessions.resumption_cue.get`).
    pub async fn get_resumption_cue(&self, key: &str) -> Result<ResumptionCueView> {
        use crate::operations::sessions::resumption_cue::get;
        self.invoke::<get::Get>(&get::Input {
            session: key.to_string(),
        })
        .await
    }

    /// Generate the resumption cue if it is missing or stale
    /// (`sessions.resumption_cue.ensure`).
    pub async fn ensure_resumption_cue(
        &self,
        key: &str,
        req: &EnsureResumptionCueReq,
    ) -> Result<ResumptionCueView> {
        use crate::operations::sessions::resumption_cue::ensure;
        self.invoke::<ensure::Ensure>(&ensure::Input {
            force: req.force,
            session: key.to_string(),
        })
        .await
    }

    /// Replace the provider behind a live ACP session while preserving the
    /// loom session, worktree, branch, and canonical journal (`sessions.handoff`).
    pub async fn handoff_session(&self, key: &str, req: &HandoffReq) -> Result<SessionView> {
        use crate::operations::sessions::handoff;
        self.invoke::<handoff::Handoff>(&handoff::Input {
            agent: req.agent.clone(),
            model: req.model.clone(),
            effort: req.effort.clone(),
            mode: req.mode.clone(),
            selection: req.selection.clone(),
            expected_profile_revision: req.expected_profile_revision,
            expected_resolver_revision: req.expected_resolver_revision.clone(),
            session: key.to_string(),
        })
        .await
    }

    /// Set (upsert) a tag on a session (`sessions.tags.set`). For a loud key
    /// (`attention` | `triage`) `value` is `attention` | `blocked`; use
    /// [`Client::clear_tag`] to return to calm rather than setting an `ok` value.
    ///
    /// The operation answers with the branch projection it wrote, so the session
    /// view this method promises is re-read afterwards.
    pub async fn set_tag(
        &self,
        key: &str,
        tag_key: &str,
        value: &str,
        note: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        use crate::operations::sessions::tags::set;
        self.invoke::<set::Set>(&set::Input {
            key: tag_key.to_string(),
            value: value.to_string(),
            note: note.to_string(),
            by: by.map(str::to_string),
            session: key.to_string(),
        })
        .await?;
        self.get_session(key).await
    }

    /// Atomically replace every tag authored by `by` on a session. Exact
    /// `(key, value)` entries in `clear` are removed in the same transaction.
    ///
    /// The only method still on a hand-written route: the operation surface has
    /// no author-scoped bulk replacement, and `sessions.tags.set`/`.delete`
    /// per key would drop both the atomicity and the "remove what this author
    /// set and no longer wants" half that the status watch depends on.
    pub async fn set_tags(&self, key: &str, req: &SetTagsReq) -> Result<SessionView> {
        self.send_typed(
            Method::PUT,
            &format!("/api/sessions/{}/tags", Self::seg(key)),
            Some(req),
        )
        .await
    }

    /// Clear a tag on a session (`sessions.tags.delete`) — how a loud axis
    /// returns to calm (`ok`). `by` attributes the clear on the audit event (a
    /// watch name); the server defaults `manual`.
    ///
    /// Re-reads the session view for the same reason as [`Client::set_tag`].
    pub async fn clear_tag(
        &self,
        key: &str,
        tag_key: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        use crate::operations::sessions::tags::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            key: tag_key.to_string(),
            by: by.map(str::to_string),
            session: key.to_string(),
        })
        .await?;
        self.get_session(key).await
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
    /// (`sessions.send`). For ACP, this steers a supported live turn and
    /// otherwise cancels it before starting a normal turn.
    pub async fn nudge(&self, key: &str, req: &SendReq) -> Result<Value> {
        use crate::operations::sessions::send;
        let result = self
            .invoke::<send::Send>(&send::Input {
                text: req.text.clone(),
                submit: Some(req.submit),
                by: req.by.clone(),
                session: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    /// Send a break (Escape) to interrupt the agent's current turn
    /// (`sessions.interrupt`).
    pub async fn interrupt(&self, key: &str) -> Result<Value> {
        use crate::operations::sessions::interrupt;
        let result = self
            .invoke::<interrupt::Interrupt>(&interrupt::Input {
                session: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    /// Capture the session's terminal pane as plain text, with `lines` of extra
    /// scrollback above the visible screen (`sessions.preview`).
    pub async fn preview(&self, key: &str, lines: usize) -> Result<String> {
        use crate::operations::sessions::preview;
        let result = self
            .invoke::<preview::Preview>(&preview::Input {
                lines: lines as i64,
                session: key.to_string(),
            })
            .await?;
        Ok(result.screen)
    }

    /// Typed, bounded worktree changes relative to the session's local base
    /// (`sessions.changes`).
    pub async fn changes(&self, key: &str) -> Result<crate::ChangeSetDto> {
        use crate::operations::sessions::changes;
        self.invoke::<changes::Changes>(&changes::Input {
            session: key.to_string(),
        })
        .await
    }

    /// Recent events for a branch, newest first, capped at 200 server-side
    /// (`branches.events.list`). `key` may be a branch id, `repo:branch`, or
    /// unambiguous id prefix — no live session required.
    pub async fn branch_log(&self, key: &str) -> Result<Vec<weaver_core::events::Event>> {
        use crate::operations::branches::events::list;
        self.invoke::<list::List>(&list::Input {
            branch: key.to_string(),
        })
        .await
    }

    // -- Channels ----------------------------------------------------------

    pub async fn list_channels(&self, archived: bool) -> Result<Vec<ChannelView>> {
        use crate::operations::channels::list;
        self.invoke::<list::List>(&list::Input {
            archived,
            branch: String::new(),
        })
        .await
    }

    pub async fn create_channel(&self, req: &CreateChannelReq) -> Result<ChannelView> {
        use crate::operations::channels::create;
        self.invoke::<create::Create>(&create::Input {
            name: req.name.clone(),
            topic: req.topic.clone(),
            repo_root: req.repo_root.clone().unwrap_or_default(),
            branch: None,
        })
        .await
    }

    pub async fn get_channel(&self, id: &str) -> Result<ChannelView> {
        use crate::operations::channels::get;
        self.invoke::<get::Get>(&get::Input {
            channel: id.to_string(),
            branch: String::new(),
        })
        .await
    }

    pub async fn channel_bindings(&self, id: &str) -> Result<Vec<ChannelBindingView>> {
        use crate::operations::channels::bindings::list;
        self.invoke::<list::List>(&list::Input {
            channel: id.to_string(),
            branch: String::new(),
        })
        .await
    }

    pub async fn channel_messages(&self, id: &str, after: i64) -> Result<Vec<ChannelMessageView>> {
        self.channel_messages_bounded(id, after, 100).await
    }

    pub async fn channel_messages_bounded(
        &self,
        id: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ChannelMessageView>> {
        use crate::operations::channels::messages::list;
        self.invoke::<list::List>(&list::Input {
            channel: id.to_string(),
            after: after.max(0),
            limit: limit as i64,
            kinds: Vec::new(),
            peek: false,
            branch: String::new(),
        })
        .await
    }

    pub async fn send_channel_message(
        &self,
        id: &str,
        req: &CreateChannelMessageReq,
    ) -> Result<ChannelMessageView> {
        use crate::operations::channels::messages::create;
        self.invoke::<create::Create>(&create::Input {
            channel: id.to_string(),
            body: req.body.clone(),
            kind: req.kind.clone(),
            urgency: req.urgency.clone(),
            payload: req.payload.clone(),
            reply_to: req.reply_to.clone(),
            idempotency_key: req.idempotency_key.clone(),
            branch: String::new(),
        })
        .await
    }

    pub async fn set_channel_subscription(
        &self,
        id: &str,
        mode: &str,
        session_id: Option<&str>,
    ) -> Result<ChannelSubscriptionView> {
        use crate::operations::channels::subscription::set;
        self.invoke::<set::Set>(&set::Input {
            channel: id.to_string(),
            mode: mode.to_string(),
            session: session_id.map(str::to_string),
            branch: String::new(),
        })
        .await
    }

    pub async fn mark_channel_read(
        &self,
        id: &str,
        seq: Option<i64>,
    ) -> Result<ChannelSubscriptionView> {
        use crate::operations::channels::read_marker::set;
        self.invoke::<set::Set>(&set::Input {
            channel: id.to_string(),
            seq,
            branch: String::new(),
        })
        .await
    }

    // -- Branches -----------------------------------------------------------

    /// Get one branch by id, `repo:branch`, or unambiguous id prefix — no live
    /// session required (`branches.get`).
    pub async fn get_branch(&self, key: &str) -> Result<BranchView> {
        use crate::operations::branches::get;
        self.invoke::<get::Get>(&get::Input {
            branch: key.to_string(),
        })
        .await
    }

    /// Set the agent's attention level and current-state message in one call
    /// (`branches.status.set`). `level` is `ok` | `attention` | `blocked`; an
    /// empty `message` leaves the previous one in place.
    pub async fn set_branch_status(
        &self,
        key: &str,
        level: &str,
        message: &str,
    ) -> Result<BranchView> {
        use crate::operations::branches::status::set;
        self.invoke::<set::Set>(&set::Input {
            level: level.to_string(),
            message: (!message.is_empty()).then(|| message.to_string()),
            branch: key.to_string(),
        })
        .await
    }

    /// Set (upsert) a tag on a branch, no live session required
    /// (`branches.tags.set`).
    pub async fn set_branch_tag(
        &self,
        key: &str,
        tag_key: &str,
        value: &str,
        note: &str,
        by: &str,
    ) -> Result<BranchView> {
        use crate::operations::branches::tags::set;
        self.invoke::<set::Set>(&set::Input {
            key: tag_key.to_string(),
            value: value.to_string(),
            note: note.to_string(),
            by: Some(by.to_string()),
            branch: key.to_string(),
        })
        .await
    }

    /// Clear a tag on a branch, no live session required
    /// (`branches.tags.delete`).
    pub async fn clear_branch_tag(&self, key: &str, tag_key: &str, by: &str) -> Result<BranchView> {
        use crate::operations::branches::tags::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            key: tag_key.to_string(),
            by: Some(by.to_string()),
            branch: key.to_string(),
        })
        .await
    }

    /// Append a raw event row to a branch's log — the escape hatch for an
    /// event kind with no dedicated mutating operation (e.g. an agent hook)
    /// (`branches.events.create`).
    pub async fn record_branch_event(&self, key: &str, kind: &str, data: Value) -> Result<Value> {
        use crate::operations::branches::events::create;
        let event = self
            .invoke::<create::Create>(&create::Input {
                kind: kind.to_string(),
                data,
                branch: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(event)?)
    }

    // -- Branch-scoped artifacts ---------------------------------------------
    //
    // The `artifacts.*` operations are addressed by branch, not by session:
    // what the `loom artifacts` CLI needs, since it may target a branch with no
    // active session.

    /// List a branch's artifacts — its own plus repo-shared, or (`repo: true`)
    /// every artifact in the repo regardless of scope (`artifacts.list`).
    pub async fn list_branch_artifacts(&self, key: &str, repo: bool) -> Result<Vec<ArtifactMeta>> {
        use crate::operations::artifacts::list;
        self.invoke::<list::List>(&list::Input {
            repo,
            branch: key.to_string(),
        })
        .await
    }

    /// Fetch an artifact's content. By default resolves branch-scoped first
    /// then repo-shared (what `show` displays); `repo: true` targets the
    /// repo-shared row of this name specifically. `rev` selects a revision;
    /// `None` is the latest (`artifacts.get`).
    pub async fn get_branch_artifact(
        &self,
        key: &str,
        name: &str,
        rev: Option<i64>,
        repo: bool,
    ) -> Result<ArtifactView> {
        use crate::operations::artifacts::get;
        self.invoke::<get::Get>(&get::Input {
            name: name.to_string(),
            rev,
            repo,
            branch: key.to_string(),
        })
        .await
    }

    /// Write a new revision of an artifact, creating it if absent
    /// (`artifacts.write`). `author` is dropped: the operation derives the
    /// writer from the credential.
    pub async fn write_branch_artifact(
        &self,
        key: &str,
        name: &str,
        req: &ArtifactUpsertReq,
    ) -> Result<ArtifactView> {
        use crate::operations::artifacts::write;
        self.invoke::<write::Write>(&write::Input {
            name: name.to_string(),
            content: req.content.clone(),
            title: req.title.clone(),
            kind: req.kind.clone(),
            base_rev: req.base_rev,
            repo: req.repo,
            branch: key.to_string(),
        })
        .await
    }

    /// The dashboard deep-link for a branch artifact, resolved server-side
    /// (`artifacts.url`) so it carries the externally-visible origin
    /// (`auth.base_url`, else the request Host) rather than the loopback/wildcard
    /// address the agent dials — a `0.0.0.0` link is useless to whoever reads it.
    /// See `loom session url` for the same pattern.
    pub async fn branch_artifact_url(&self, key: &str, name: &str) -> Result<String> {
        use crate::operations::artifacts::url;
        let view = self
            .invoke::<url::Url>(&url::Input {
                name: name.to_string(),
                branch: key.to_string(),
            })
            .await?;
        Ok(view.url)
    }

    /// Delete an artifact and its whole revision history. `repo: true` targets
    /// the repo-shared row of this name rather than the branch-scoped one
    /// (`artifacts.delete`).
    pub async fn delete_branch_artifact(&self, key: &str, name: &str, repo: bool) -> Result<Value> {
        use crate::operations::artifacts::delete;
        let result = self
            .invoke::<delete::Delete>(&delete::Input {
                name: name.to_string(),
                repo,
                branch: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    // -- Branch-scoped discussion ---------------------------------------------
    //
    // Addressed by branch for `loom artifacts comment/resolve/threads`, which —
    // like every other `weaver` command — needs no live session.

    /// Every thread on an artifact, open and resolved, each with its comments
    /// (`artifacts.threads.list`).
    pub async fn list_branch_threads(&self, key: &str, name: &str) -> Result<Vec<ThreadDto>> {
        use crate::operations::artifacts::threads::list;
        self.invoke::<list::List>(&list::Input {
            name: name.to_string(),
            open_only: false,
            branch: key.to_string(),
        })
        .await
    }

    /// Open a new thread anchored to a quoted span, seeded with its first
    /// comment (`artifacts.threads.comment`, `target: {"kind": "new", ...}`).
    pub async fn create_branch_thread(
        &self,
        key: &str,
        name: &str,
        base_rev: i64,
        anchor: AnchorDto,
        body: &str,
    ) -> Result<ThreadDto> {
        use crate::operations::artifacts::threads::comment;
        self.invoke::<comment::Comment>(&comment::Input {
            name: name.to_string(),
            body: body.to_string(),
            target: comment::CommentTarget::New { base_rev, anchor },
            branch: key.to_string(),
        })
        .await
    }

    /// Append a reply to an existing thread (`artifacts.threads.comment`,
    /// `target: {"kind": "reply", ...}`). The operation answers with the whole
    /// thread for both targets, so the appended reply is its last comment.
    pub async fn add_branch_thread_comment(
        &self,
        key: &str,
        name: &str,
        thread_id: i64,
        body: &str,
    ) -> Result<CommentDto> {
        use crate::operations::artifacts::threads::comment;
        let thread = self
            .invoke::<comment::Comment>(&comment::Input {
                name: name.to_string(),
                body: body.to_string(),
                target: comment::CommentTarget::Reply { thread_id },
                branch: key.to_string(),
            })
            .await?;
        thread
            .comments
            .into_iter()
            .next_back()
            .ok_or_else(|| anyhow!("thread {thread_id} carries no comment after the reply"))
    }

    /// Mark a thread resolved (`artifacts.threads.resolve`).
    pub async fn resolve_branch_thread(
        &self,
        key: &str,
        name: &str,
        thread_id: i64,
    ) -> Result<Value> {
        use crate::operations::artifacts::threads::resolve;
        let thread = self
            .invoke::<resolve::Resolve>(&resolve::Input {
                name: name.to_string(),
                thread_id,
                branch: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(thread)?)
    }

    // -- Staged reviews ------------------------------------------------------

    /// A session's reviews for one subject — an artifact, or its change-set
    /// (`reviews.list`).
    pub async fn list_session_reviews(
        &self,
        session: &str,
        subject_kind: &str,
        subject_key: &str,
    ) -> Result<Vec<ReviewDto>> {
        use crate::operations::reviews::list;
        self.invoke::<list::List>(&list::Input {
            subject_kind: review_subject_kind(subject_kind)?,
            subject_key: subject_key.to_string(),
            session: session.to_string(),
        })
        .await
    }

    pub async fn create_session_review(
        &self,
        session: &str,
        req: &CreateReviewReq,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::create;
        self.invoke::<create::Create>(&create::Input {
            session: session.to_string(),
            subject_kind: req.subject_kind,
            subject_key: req.subject_key.clone(),
            subject_version: req.subject_version.clone(),
        })
        .await
    }

    pub async fn add_review_comment(
        &self,
        review_id: i64,
        req: &AddReviewCommentReq,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::comments::create;
        self.invoke::<create::Create>(&create::Input {
            id: review_id,
            expected_revision: req.expected_revision,
            subject_version: req.subject_version.clone(),
            anchor_kind: req.anchor_kind,
            anchor: req.anchor.clone(),
            body: req.body.clone(),
        })
        .await
    }

    pub async fn get_review(&self, review_id: i64) -> Result<ReviewDto> {
        use crate::operations::reviews::get;
        self.invoke::<get::Get>(&get::Input { id: review_id }).await
    }

    pub async fn update_review_comment(
        &self,
        review_id: i64,
        comment_id: i64,
        req: &UpdateReviewCommentReq,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::comments::update;
        self.invoke::<update::Update>(&update::Input {
            id: review_id,
            comment_id,
            expected_revision: req.expected_revision,
            body: req.body.clone(),
            subject_version: req.subject_version.clone(),
            anchor_kind: req.anchor_kind,
            anchor: req.anchor.clone(),
        })
        .await
    }

    pub async fn update_review(&self, review_id: i64, req: &UpdateReviewReq) -> Result<ReviewDto> {
        use crate::operations::reviews::update;
        self.invoke::<update::Update>(&update::Input {
            id: review_id,
            expected_revision: req.expected_revision,
            summary: req.summary.clone(),
            subject_version: req.subject_version.clone(),
        })
        .await
    }

    pub async fn delete_review_comment(
        &self,
        review_id: i64,
        comment_id: i64,
        expected_revision: i64,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::comments::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            id: review_id,
            comment_id,
            expected_revision,
        })
        .await
    }

    pub async fn discard_review(&self, review_id: i64, expected_revision: i64) -> Result<Value> {
        use crate::operations::reviews::discard;
        let result = self
            .invoke::<discard::Discard>(&discard::Input {
                id: review_id,
                expected_revision,
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    pub async fn submit_review(&self, review_id: i64, req: &SubmitReviewReq) -> Result<ReviewDto> {
        use crate::operations::reviews::submit;
        self.invoke::<submit::Submit>(&submit::Input {
            id: review_id,
            expected_revision: req.expected_revision,
            acknowledge_outdated: req.acknowledge_outdated,
        })
        .await
    }

    pub async fn retarget_review_to_current(
        &self,
        review_id: i64,
        expected_revision: i64,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::retarget;
        self.invoke::<retarget::Retarget>(&retarget::Input {
            id: review_id,
            expected_revision,
        })
        .await
    }

    pub async fn set_review_comment_resolution(
        &self,
        review_id: i64,
        comment_id: i64,
        resolved: bool,
    ) -> Result<ReviewCommentDto> {
        use crate::operations::reviews::comments::resolve;
        self.invoke::<resolve::Resolve>(&resolve::Input {
            id: review_id,
            comment_id,
            resolved,
        })
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
        use crate::operations::reviews::retry_delivery;
        self.invoke::<retry_delivery::RetryDelivery>(&retry_delivery::Input { id: review_id })
            .await
    }

    // -- Issues ---------------------------------------------------------------

    /// Patch a work item's title/body/status/GitHub mapping (`issues.update`).
    ///
    /// `PatchIssueReq::claimed_branch` had exactly one legal value — `null`,
    /// meaning "return this to the backlog" — which the operation spells as the
    /// `unclaim` operand. Naming a branch is still rejected: a claim is made by
    /// launching a session against the item.
    pub async fn patch_issue(&self, id: i64, req: &PatchIssueReq) -> Result<IssueView> {
        use crate::operations::issues::update;
        if req
            .claimed_branch
            .as_ref()
            .and_then(|branch| branch.as_deref())
            .is_some_and(|branch| !branch.trim().is_empty())
        {
            bail!("claimed_branch can only be cleared; launch a session to claim an issue");
        }
        self.invoke::<update::Update>(&update::Input {
            id,
            title: req.title.clone(),
            body: req.body.clone(),
            status: req.status.clone(),
            github: req.github.clone(),
            unclaim: req.claimed_branch.is_some(),
            repo_root: String::new(),
        })
        .await
    }

    // -- Settings -------------------------------------------------------------

    /// Public database and migration readiness (`GET /api/ready`).
    pub async fn readiness(&self) -> Result<ReadinessView> {
        self.get_typed("/api/ready").await
    }

    /// Human-readable redacted operational inventory (`diagnostics.get`).
    pub async fn diagnostics(&self) -> Result<DiagnosticsView> {
        use crate::operations::diagnostics::get;
        self.invoke::<get::Get>(&get::Input {}).await
    }

    /// Trusted MCP adapters and their provider-neutral capability sets
    /// (`mcps.get`).
    pub async fn mcp_registry(&self) -> Result<McpRegistryView> {
        use crate::operations::mcps::get;
        self.invoke::<get::Get>(&get::Input {}).await
    }

    pub async fn create_custom_mcp(&self, req: &CustomMcpReq) -> Result<CustomMcpView> {
        use crate::operations::mcps::custom::create;
        self.invoke::<create::Create>(&create::Input {
            identity: req.identity.clone(),
            label: req.label.clone(),
            description: req.description.clone(),
            source: req.source.clone(),
            test_source: req.test_source.clone(),
            enabled: req.enabled,
        })
        .await
    }

    pub async fn put_custom_mcp(
        &self,
        identity: &str,
        req: &CustomMcpReq,
    ) -> Result<CustomMcpView> {
        use crate::operations::mcps::custom::update;
        self.invoke::<update::Update>(&update::Input {
            identity: identity.to_string(),
            label: req.label.clone(),
            description: req.description.clone(),
            source: req.source.clone(),
            test_source: req.test_source.clone(),
            enabled: req.enabled,
        })
        .await
    }

    pub async fn delete_custom_mcp(&self, identity: &str) -> Result<Value> {
        use crate::operations::mcps::custom::delete;
        let result = self
            .invoke::<delete::Delete>(&delete::Input {
                identity: identity.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    /// List named launch profiles. Secret environment values are withheld
    /// (`profiles.list`).
    pub async fn list_profiles(&self) -> Result<Vec<ProfileView>> {
        use crate::operations::profiles::list;
        self.invoke::<list::List>(&list::Input {}).await
    }

    pub async fn get_profile(&self, name: &str) -> Result<ProfileView> {
        use crate::operations::profiles::get;
        self.invoke::<get::Get>(&get::Input {
            name: name.to_string(),
        })
        .await
    }

    pub async fn effective_profile(&self, name: &str) -> Result<EffectiveProfileView> {
        use crate::operations::profiles::effective;
        self.invoke::<effective::Effective>(&effective::Input {
            name: name.to_string(),
        })
        .await
    }

    pub async fn create_profile(&self, req: &ProfileReq) -> Result<ProfileView> {
        use crate::operations::profiles::create;
        self.invoke::<create::Create>(&create::Input {
            name: req.name.clone(),
            description: req.description.clone(),
            agent_kind: req.agent_kind.clone(),
            model: req.model.clone(),
            effort: req.effort.clone(),
            protocol: req.protocol.clone(),
            mode: req.mode.clone(),
            class: req.class.clone(),
            strict: req.strict,
            env_clear: req.env_clear,
            ambient_allowlist: req.ambient_allowlist.clone(),
            idle_archive_secs: req.idle_archive_secs,
            max_concurrent: req.max_concurrent,
            turn_budget: req.turn_budget,
            prelude: req.prelude.clone(),
            instructions: req.instructions.clone(),
            restricted: req.restricted,
            github_repositories: req.github_repositories.clone(),
            runtime_permissions: req.runtime_permissions.clone(),
            mcp_access: req.mcp_access.clone(),
        })
        .await
    }

    pub async fn put_profile(&self, name: &str, req: &ProfileReq) -> Result<ProfileView> {
        use crate::operations::profiles::update;
        self.invoke::<update::Update>(&update::Input {
            name: name.to_string(),
            description: req.description.clone(),
            agent_kind: req.agent_kind.clone(),
            model: req.model.clone(),
            effort: req.effort.clone(),
            protocol: req.protocol.clone(),
            mode: req.mode.clone(),
            class: req.class.clone(),
            strict: req.strict,
            env_clear: req.env_clear,
            ambient_allowlist: req.ambient_allowlist.clone(),
            idle_archive_secs: req.idle_archive_secs,
            max_concurrent: req.max_concurrent,
            turn_budget: req.turn_budget,
            prelude: req.prelude.clone(),
            instructions: req.instructions.clone(),
            restricted: req.restricted,
            github_repositories: req.github_repositories.clone(),
            runtime_permissions: req.runtime_permissions.clone(),
            mcp_access: req.mcp_access.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    /// Clone one profile's policy on the server, optionally copying its
    /// write-only environment in the same transaction (`profiles.clone`).
    pub async fn clone_profile(&self, source: &str, req: &CloneProfileReq) -> Result<ProfileView> {
        use crate::operations::profiles::clone;
        self.invoke::<clone::Clone>(&clone::Input {
            source: source.to_string(),
            name: req.name.clone(),
            expected_profile_revision: req.expected_profile_revision,
            expected_resolver_revision: req.expected_resolver_revision.clone(),
            overrides: req.overrides.clone(),
            template: req.template.clone(),
            copy_environment: req.copy_environment,
            environment: req.environment.clone(),
        })
        .await
    }

    /// Return the upload limits shared by launch and live-session Scratch
    /// (`sessions.scratch.limits`).
    pub async fn scratch_limits(&self) -> Result<ScratchLimitsView> {
        use crate::operations::sessions::scratch::limits;
        self.invoke::<limits::Limits>(&limits::Input {}).await
    }

    pub async fn delete_profile(&self, name: &str) -> Result<Value> {
        use crate::operations::profiles::delete;
        let result = self
            .invoke::<delete::Delete>(&delete::Input {
                name: name.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    pub async fn set_profile_env(
        &self,
        profile: &str,
        name: &str,
        value: &str,
    ) -> Result<ProfileView> {
        use crate::operations::profiles::env::set;
        self.invoke::<set::Set>(&set::Input {
            profile: profile.to_string(),
            name: name.to_string(),
            value: Some(value.to_string()),
            secret_ref: None,
        })
        .await
    }

    pub async fn set_profile_env_secret(
        &self,
        profile: &str,
        name: &str,
        secret_ref: &str,
    ) -> Result<ProfileView> {
        use crate::operations::profiles::env::set;
        self.invoke::<set::Set>(&set::Input {
            profile: profile.to_string(),
            name: name.to_string(),
            value: None,
            secret_ref: Some(secret_ref.to_string()),
        })
        .await
    }

    pub async fn remove_profile_env(&self, profile: &str, name: &str) -> Result<ProfileView> {
        use crate::operations::profiles::env::delete;
        self.invoke::<delete::Delete>(&delete::Input {
            profile: profile.to_string(),
            name: name.to_string(),
        })
        .await
    }

    /// Every registered setting and its effective value (`settings.get`).
    pub async fn list_settings(&self) -> Result<SettingsEnvelope> {
        use crate::operations::settings::get;
        self.invoke::<get::Get>(&get::Input {}).await
    }

    /// Apply setting changes: a `null` value clears a key back to its default
    /// (`settings.patch`).
    pub async fn patch_settings(
        &self,
        changes: serde_json::Map<String, Value>,
    ) -> Result<SettingsEnvelope> {
        use crate::operations::settings::patch;
        self.invoke::<patch::Patch>(&patch::Input {
            changes: changes
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect(),
        })
        .await
    }

    // -- Watches ------------------------------------------------------

    /// List every watch (`watches.list`).
    pub async fn list_watches(&self) -> Result<Vec<WatchView>> {
        use crate::operations::watches::list;
        self.invoke::<list::List>(&list::Input {}).await
    }

    /// Get one watch by id or name (`watches.get`).
    pub async fn get_watch(&self, key: &str) -> Result<WatchView> {
        use crate::operations::watches::get;
        self.invoke::<get::Get>(&get::Input {
            key: key.to_string(),
        })
        .await
    }

    /// Register a watch (`watches.create`).
    pub async fn create_watch(&self, req: &CreateWatchReq) -> Result<WatchView> {
        use crate::operations::watches::create;
        self.invoke::<create::Create>(&create::Input {
            name: req.name.clone(),
            trigger: req.trigger.clone(),
            scope: req.scope.clone(),
            program: req.program.clone(),
            params: req.params.clone(),
            capabilities: req.capabilities.clone(),
            profile: req.profile.clone(),
            model: req.model.clone(),
            effort: req.effort.clone(),
            cooldown_secs: req.cooldown_secs,
            enabled: req.enabled,
        })
        .await
    }

    /// Patch a watch (`watches.update`).
    pub async fn patch_watch(&self, key: &str, req: &PatchWatchReq) -> Result<WatchView> {
        use crate::operations::watches::update;
        self.invoke::<update::Update>(&update::Input {
            key: key.to_string(),
            enabled: req.enabled,
            trigger: req.trigger.clone(),
            scope: req.scope.clone(),
            program: req.program.clone(),
            params: req.params.clone(),
            capabilities: req.capabilities.clone(),
            profile: req.profile.clone(),
            model: req.model.clone(),
            effort: req.effort.clone(),
            cooldown_secs: req.cooldown_secs,
        })
        .await
    }

    /// Delete a watch (`watches.delete`).
    pub async fn delete_watch(&self, key: &str) -> Result<Value> {
        use crate::operations::watches::delete;
        let result = self
            .invoke::<delete::Delete>(&delete::Input {
                key: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    /// Fire a round now and return the raw `{run_id, outcome, summary}`
    /// (`watches.run`).
    pub async fn run_watch(&self, key: &str, req: &RunWatchReq) -> Result<Value> {
        use crate::operations::watches::run;
        let result = self
            .invoke::<run::Run>(&run::Input {
                key: key.to_string(),
                dry_run: req.dry_run,
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    // -- API tokens -------------------------------------------------------

    pub async fn mint_automation_token(
        &self,
        req: &AutomationTokenReq,
    ) -> Result<AutomationTokenView> {
        use crate::operations::auth::automation_token;
        self.invoke::<automation_token::AutomationToken>(&automation_token::Input {
            subject: req.subject.clone(),
            profiles: req.profiles.clone(),
            ttl_secs: req.ttl_secs,
        })
        .await
    }

    pub async fn list_federations(&self) -> Result<Vec<FederationView>> {
        use crate::operations::auth::federations::list;
        self.invoke::<list::List>(&list::Input {}).await
    }

    pub async fn add_federation(&self, req: &FederationReq) -> Result<FederationView> {
        use crate::operations::auth::federations::create;
        self.invoke::<create::Create>(&create::Input {
            name: Some(req.name.clone()),
            provider: req.provider.clone(),
            issuer: req.issuer.clone(),
            audience: req.audience.clone(),
            subject: req.subject.clone(),
            service_account: req.service_account.clone(),
            service_tag: req.service_tag.clone(),
            repository_id: req.repository_id.clone(),
            workflow_ref: req.workflow_ref.clone(),
            event_name: req.event_name.clone(),
            ref_pattern: req.ref_pattern.clone(),
            profiles: req.profiles.clone(),
        })
        .await
    }

    pub async fn remove_federation(&self, id: &str) -> Result<Value> {
        use crate::operations::auth::federations::remove;
        let result = self
            .invoke::<remove::Remove>(&remove::Input { id: id.to_string() })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    pub async fn reconcile_deployment(&self, req: &DeploymentReq) -> Result<DeploymentView> {
        use crate::operations::deployment::reconcile;
        self.invoke::<reconcile::Reconcile>(&reconcile::Input {
            settings: req.settings.clone(),
            profiles: req.profiles.clone(),
            federations: req.federations.clone(),
            prune: req.prune,
        })
        .await
    }

    pub async fn create_run(&self, req: &RunReq) -> Result<RunView> {
        use crate::operations::runs::create;
        self.invoke::<create::Create>(&create::Input {
            profile: req.profile.clone(),
            idempotency_key: req.idempotency_key.clone(),
            source: req.source.clone(),
            watch_id: req.watch_id.clone(),
            channel: req.channel.clone(),
            slack: req.slack.clone(),
            session: req.session.clone(),
        })
        .await
    }

    /// List the user-managed API tokens (`auth.tokens.list`).
    pub async fn list_tokens(&self) -> Result<Vec<TokenView>> {
        use crate::operations::auth::tokens::list;
        self.invoke::<list::List>(&list::Input {}).await
    }

    /// Mint a new API token, returning the one-time plaintext
    /// (`auth.tokens.create`).
    pub async fn create_token(&self, req: &CreateTokenReq) -> Result<CreatedTokenView> {
        use crate::operations::auth::tokens::create;
        self.invoke::<create::Create>(&create::Input {
            name: req.name.clone(),
            expires_in_days: req.expires_in_days,
        })
        .await
    }

    /// Revoke an API token by id (`auth.tokens.revoke`).
    pub async fn revoke_token(&self, id: &str) -> Result<Value> {
        use crate::operations::auth::tokens::revoke;
        let result = self
            .invoke::<revoke::Revoke>(&revoke::Input { id: id.to_string() })
            .await?;
        Ok(serde_json::to_value(result)?)
    }
}

/// Parse the wire spelling of a review subject kind, which
/// [`Client::list_session_reviews`] still takes as a plain string.
fn review_subject_kind(kind: &str) -> Result<ReviewSubjectKindDto> {
    match kind.trim() {
        "artifact" => Ok(ReviewSubjectKindDto::Artifact),
        "changes" => Ok(ReviewSubjectKindDto::Changes),
        other => Err(anyhow!("unknown review subject kind `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::Client;

    /// The client and the registry must derive the same route for every
    /// operation, or a typed call reaches a path the server does not serve.
    ///
    /// This is the check that was missing when `invoke_value` percent-encoded id
    /// segments: 27 of the 153 operations have an underscore in their id, and
    /// every one of them was silently unreachable through this client.
    #[test]
    fn the_client_derives_the_same_path_as_the_registry() {
        let mut checked = 0;
        for operation in crate::operations::operations() {
            assert_eq!(
                Client::operation_path(operation.id),
                operation.path(),
                "client and registry disagree about {}'s route",
                operation.id
            );
            checked += 1;
        }
        assert!(checked > 100, "only {checked} operations checked");
    }

    /// And specifically: nothing in a derived path is percent-encoded.
    #[test]
    fn no_operation_path_is_escaped() {
        for operation in crate::operations::operations() {
            let path = Client::operation_path(operation.id);
            assert!(
                !path.contains('%'),
                "{} derives an escaped path {path}",
                operation.id
            );
        }
    }
}
