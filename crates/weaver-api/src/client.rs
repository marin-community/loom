//! The typed loom REST client. A thin layer over an untyped JSON `send`: the
//! untyped `get`/`post`/`patch`/`delete` are kept for callers that pretty-print
//! raw JSON (the `loom` CLI), and the typed methods over them each invoke one
//! code-registered operation, serializing its `Input` and deserializing its
//! `Output` — the surface the Python binding wraps.

use anyhow::{anyhow, bail, Result};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::operations::{ApiMetaView, Operation, OperationView};

use crate::dto::{
    AddReviewCommentReq, ArtifactUpsertReq, ArtifactView, AutomationTokenReq, AutomationTokenView,
    ChannelMessageView, ChannelView, CloneProfileReq, CommentDto, CreateChannelMessageReq,
    CreateChannelReq, CreateReq, CreateReviewReq, CreateTokenReq, CreatedTokenView, CustomMcpReq,
    CustomMcpView, DecidePermissionRequestReq, DeploymentReq, DeploymentView, FederationReq,
    FederationView, HandoffReq, MoveSessionsReq, PermissionRequestView, ProfileReq, ProfileView,
    ReadinessView, ResolveLaunchReq, ResolvedLaunchView, ReviewDto, RunReq, RunView, RunWatchReq,
    SearchSessionsOptions, SendReq, SessionGithubAccessView, SessionLayoutView, SessionView,
    SetSessionGithubAccessReq, SubmitReviewReq, UpdateReviewCommentReq, UpdateReviewReq,
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
    /// it. Derived as a plain substitution: dots become slashes, no percent-encoding.
    /// An operation id is `[a-z0-9_]` joined by dots and cannot contain a slash.
    fn operation_path(id: &str) -> String {
        format!("/api/{}", id.replace('.', "/"))
    }

    // -- Sessions ---------------------------------------------------------

    /// Discover the connected Loom server and its operation registry version.
    pub async fn api_meta(&self) -> Result<ApiMetaView> {
        self.get_typed("/api/meta").await
    }

    /// List the transport-neutral operation catalogue advertised by the server.
    pub async fn operations(&self) -> Result<Vec<OperationView>> {
        self.get_typed("/api/operations").await
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
            self.invoke::<grant::Op>(&grant::Input {
                repository: request.repository.clone(),
                session: session_id.to_string(),
            })
            .await
        } else {
            self.invoke::<revoke::Op>(&revoke::Input {
                repository: request.repository.clone(),
                session: session_id.to_string(),
            })
            .await
        }
    }

    /// Approve or deny a pending permission request.
    ///
    /// Approving carries `risk = ExternalWrite`; denying is an ordinary write.
    /// The choice of operation determines the risk level.
    pub async fn decide_permission_request(
        &self,
        request_id: &str,
        request: &DecidePermissionRequestReq,
    ) -> Result<PermissionRequestView> {
        use crate::operations::permissions::requests::{approve, deny};
        match request.decision.trim() {
            "approve" => {
                self.invoke::<approve::Op>(&approve::Input {
                    request: request_id.to_string(),
                    reason: request.reason.clone(),
                })
                .await
            }
            "deny" => {
                self.invoke::<deny::Op>(&deny::Input {
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
        use crate::operations::sessions::list;
        let options = SearchSessionsOptions::default();
        self.invoke::<list::Op>(&list::Input {
            q: options.query,
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

    pub async fn move_sessions(&self, req: &MoveSessionsReq) -> Result<SessionLayoutView> {
        use crate::operations::session_layout::r#move;
        self.invoke::<r#move::Op>(&r#move::Input {
            session_ids: req.session_ids.clone(),
            destination_group_id: req.destination_group_id.clone(),
            before_session_id: req.before_session_id.clone(),
            expected_revision: req.expected_revision,
        })
        .await
    }

    /// Launch a new session (`sessions.launch`).
    pub async fn create_session(&self, req: &CreateReq) -> Result<SessionView> {
        use crate::operations::sessions::launch;
        self.invoke::<launch::Op>(&launch::Input {
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
        self.invoke::<resolve::Op>(&resolve::Input {
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
        self.invoke::<resolve::Op>(&resolve::Input {
            selection: req.selection.clone(),
            session: key.to_string(),
        })
        .await
    }

    /// Replace the provider behind a live ACP session while preserving the
    /// loom session, worktree, branch, and canonical journal (`sessions.handoff`).
    pub async fn handoff_session(&self, key: &str, req: &HandoffReq) -> Result<SessionView> {
        use crate::operations::sessions::handoff;
        self.invoke::<handoff::Op>(&handoff::Input {
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

    /// Stamp a watch's mark on a session — the `triage` tag. A convenience
    /// over [`Client::set_tag`] / [`Client::clear_tag`] that keeps the `mark`
    /// capability name: a `level` of `attention`/`blocked` sets the tag, an empty
    /// `level` (or `ok`) clears it.
    /// Set one session tag and return the session as it now reads.
    ///
    /// Two round trips, not one: `sessions.tags.set` answers with the branch,
    /// and callers of this want the session.
    pub async fn set_tag(
        &self,
        key: &str,
        tag_key: &str,
        value: &str,
        note: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        use crate::operations::sessions::{get, tags::set};
        self.invoke::<set::Op>(&set::Input {
            key: tag_key.to_string(),
            value: value.to_string(),
            note: note.to_string(),
            by: by.map(str::to_string),
            session: key.to_string(),
        })
        .await?;
        self.invoke::<get::Op>(&get::Input {
            session: key.to_string(),
        })
        .await
    }

    /// Clear one session tag and return the session as it now reads.
    pub async fn clear_tag(
        &self,
        key: &str,
        tag_key: &str,
        by: Option<&str>,
    ) -> Result<SessionView> {
        use crate::operations::sessions::{get, tags::delete};
        self.invoke::<delete::Op>(&delete::Input {
            key: tag_key.to_string(),
            by: by.map(str::to_string),
            session: key.to_string(),
        })
        .await?;
        self.invoke::<get::Op>(&get::Input {
            session: key.to_string(),
        })
        .await
    }

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
            .invoke::<send::Op>(&send::Input {
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
            .invoke::<interrupt::Op>(&interrupt::Input {
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
            .invoke::<preview::Op>(&preview::Input {
                lines: lines as i64,
                session: key.to_string(),
            })
            .await?;
        Ok(result.screen)
    }

    // -- Channels ----------------------------------------------------------

    pub async fn create_channel(&self, req: &CreateChannelReq) -> Result<ChannelView> {
        use crate::operations::channels::create;
        self.invoke::<create::Op>(&create::Input {
            name: req.name.clone(),
            topic: req.topic.clone(),
            repo_root: req.repo_root.clone().unwrap_or_default(),
            branch: None,
        })
        .await
    }

    pub async fn channel_messages(&self, id: &str, after: i64) -> Result<Vec<ChannelMessageView>> {
        use crate::operations::channels::messages::list;
        self.invoke::<list::Op>(&list::Input {
            channel: id.to_string(),
            after: after.max(0),
            limit: 100,
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
        self.invoke::<create::Op>(&create::Input {
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

    // -- Branches -----------------------------------------------------------

    /// Append a raw event row to a branch's log — the escape hatch for an
    /// event kind with no dedicated mutating operation (e.g. an agent hook)
    /// (`branches.events.create`).
    pub async fn record_branch_event(&self, key: &str, kind: &str, data: Value) -> Result<Value> {
        use crate::operations::branches::events::create;
        let event = self
            .invoke::<create::Op>(&create::Input {
                kind: kind.to_string(),
                data,
                branch: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(event)?)
    }

    // -- Branch-scoped artifacts ---------------------------------------------
    //
    // The `artifacts.*` operations are addressed by branch, enabling the
    // `loom artifacts` CLI to target a branch with no active session.

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
        self.invoke::<write::Op>(&write::Input {
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
    /// (`artifacts.url`) using the externally-visible origin (`auth.base_url` or
    /// request Host), not the loopback address the agent dials.
    /// See `loom session url` for the same pattern.
    pub async fn branch_artifact_url(&self, key: &str, name: &str) -> Result<String> {
        use crate::operations::artifacts::url;
        let view = self
            .invoke::<url::Op>(&url::Input {
                name: name.to_string(),
                branch: key.to_string(),
            })
            .await?;
        Ok(view.url)
    }

    /// Delete an artifact and its whole revision history. `repo: true` targets
    /// the repo-shared row; `false` targets the branch-scoped one (`artifacts.delete`).
    pub async fn delete_branch_artifact(&self, key: &str, name: &str, repo: bool) -> Result<Value> {
        use crate::operations::artifacts::delete;
        let result = self
            .invoke::<delete::Op>(&delete::Input {
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
            .invoke::<comment::Op>(&comment::Input {
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
            .invoke::<resolve::Op>(&resolve::Input {
                name: name.to_string(),
                thread_id,
                branch: key.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(thread)?)
    }

    // -- Staged reviews ------------------------------------------------------

    pub async fn create_session_review(
        &self,
        session: &str,
        req: &CreateReviewReq,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::create;
        self.invoke::<create::Op>(&create::Input {
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
        self.invoke::<create::Op>(&create::Input {
            id: review_id,
            expected_revision: req.expected_revision,
            subject_version: req.subject_version.clone(),
            anchor_kind: req.anchor_kind,
            anchor: req.anchor.clone(),
            body: req.body.clone(),
        })
        .await
    }

    pub async fn update_review_comment(
        &self,
        review_id: i64,
        comment_id: i64,
        req: &UpdateReviewCommentReq,
    ) -> Result<ReviewDto> {
        use crate::operations::reviews::comments::update;
        self.invoke::<update::Op>(&update::Input {
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
        self.invoke::<update::Op>(&update::Input {
            id: review_id,
            expected_revision: req.expected_revision,
            summary: req.summary.clone(),
            subject_version: req.subject_version.clone(),
        })
        .await
    }

    pub async fn discard_review(&self, review_id: i64, expected_revision: i64) -> Result<Value> {
        use crate::operations::reviews::discard;
        let result = self
            .invoke::<discard::Op>(&discard::Input {
                id: review_id,
                expected_revision,
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    pub async fn submit_review(&self, review_id: i64, req: &SubmitReviewReq) -> Result<ReviewDto> {
        use crate::operations::reviews::submit;
        self.invoke::<submit::Op>(&submit::Input {
            id: review_id,
            expected_revision: req.expected_revision,
            acknowledge_outdated: req.acknowledge_outdated,
        })
        .await
    }

    // -- Issues ---------------------------------------------------------------

    // -- Settings -------------------------------------------------------------

    /// Public database and migration readiness (`GET /api/ready`).
    pub async fn readiness(&self) -> Result<ReadinessView> {
        self.get_typed("/api/ready").await
    }

    pub async fn create_custom_mcp(&self, req: &CustomMcpReq) -> Result<CustomMcpView> {
        use crate::operations::mcps::custom::create;
        self.invoke::<create::Op>(&create::Input {
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
        self.invoke::<update::Op>(&update::Input {
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
            .invoke::<delete::Op>(&delete::Input {
                identity: identity.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    pub async fn create_profile(&self, req: &ProfileReq) -> Result<ProfileView> {
        use crate::operations::profiles::create;
        self.invoke::<create::Op>(&create::Input {
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

    /// Clone one profile's policy on the server, optionally copying its
    /// write-only environment in the same transaction (`profiles.clone`).
    pub async fn clone_profile(&self, source: &str, req: &CloneProfileReq) -> Result<ProfileView> {
        use crate::operations::profiles::clone;
        self.invoke::<clone::Op>(&clone::Input {
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

    pub async fn delete_profile(&self, name: &str) -> Result<Value> {
        use crate::operations::profiles::delete;
        let result = self
            .invoke::<delete::Op>(&delete::Input {
                name: name.to_string(),
            })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    // -- Watches ------------------------------------------------------

    /// Fire a round now and return the raw `{run_id, outcome, summary}`
    /// (`watches.run`).
    pub async fn run_watch(&self, key: &str, req: &RunWatchReq) -> Result<Value> {
        use crate::operations::watches::run;
        let result = self
            .invoke::<run::Op>(&run::Input {
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
        self.invoke::<automation_token::Op>(&automation_token::Input {
            subject: req.subject.clone(),
            profiles: req.profiles.clone(),
            ttl_secs: req.ttl_secs,
        })
        .await
    }

    pub async fn add_federation(&self, req: &FederationReq) -> Result<FederationView> {
        use crate::operations::auth::federations::create;
        self.invoke::<create::Op>(&create::Input {
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
            .invoke::<remove::Op>(&remove::Input { id: id.to_string() })
            .await?;
        Ok(serde_json::to_value(result)?)
    }

    pub async fn reconcile_deployment(&self, req: &DeploymentReq) -> Result<DeploymentView> {
        use crate::operations::deployment::reconcile;
        self.invoke::<reconcile::Op>(&reconcile::Input {
            settings: req.settings.clone(),
            profiles: req.profiles.clone(),
            federations: req.federations.clone(),
            prune: req.prune,
        })
        .await
    }

    pub async fn create_run(&self, req: &RunReq) -> Result<RunView> {
        use crate::operations::runs::create;
        self.invoke::<create::Op>(&create::Input {
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

    /// Mint a new API token, returning the one-time plaintext
    /// (`auth.tokens.create`).
    pub async fn create_token(&self, req: &CreateTokenReq) -> Result<CreatedTokenView> {
        use crate::operations::auth::tokens::create;
        self.invoke::<create::Op>(&create::Input {
            name: req.name.clone(),
            expires_in_days: req.expires_in_days,
        })
        .await
    }

    /// Revoke an API token by id (`auth.tokens.revoke`).
    pub async fn revoke_token(&self, id: &str) -> Result<Value> {
        use crate::operations::auth::tokens::revoke;
        let result = self
            .invoke::<revoke::Op>(&revoke::Input { id: id.to_string() })
            .await?;
        Ok(serde_json::to_value(result)?)
    }
}

/// Parse the wire spelling of a review subject kind, which
/// [`Client::list_session_reviews`] still takes as a plain string.
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
