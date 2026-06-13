//! The cross-process wire contract: the request and response (View) DTOs the
//! loom REST API speaks. These are the single source of truth — the loom server
//! serializes them, the typed [`crate::Client`] and the future Python binding
//! deserialize them, and `frontend/types.ts` mirrors them by hand.
//!
//! The response (`*View`) types carry `from_parts` constructors that build a
//! plain wire struct from the `weaver-core` domain types (`Branch`, `Issue`,
//! `Overlooker`, …). The async server-side builders that touch the database
//! (counting open issues, joining the latest run) stay in the loom server and
//! call these once they've gathered the parts — so the wire struct has exactly
//! one definition while the DB access stays where the daemon owns it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use weaver_core::branch::Branch;
use weaver_core::github::GithubStatus;
use weaver_core::issue::Issue;
use weaver_core::overlooker::{Overlooker, OverlookerRun};
use weaver_core::tags::Tag;

// ---------------------------------------------------------------------------
// View payloads (responses)
// ---------------------------------------------------------------------------

/// One tag on a branch, as the API exposes it. A `(key, value)` annotation with
/// a reason, author, and timestamp. The well-known keys are `attention` (the
/// agent's self-report) and `triage` (an overlooker's assessment); any other key
/// is a free-form, quiet pill. Absence is the calm state — there is no `ok` tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagView {
    pub key: String,
    pub value: String,
    pub note: String,
    pub set_by: String,
    pub set_at: String,
}

impl From<&Tag> for TagView {
    fn from(t: &Tag) -> Self {
        TagView {
            key: t.key.clone(),
            value: t.value.clone(),
            note: t.note.clone(),
            set_by: t.set_by.clone(),
            set_at: t.set_at.clone(),
        }
    }
}

/// Branch with denormalized open-issue count, returned by `/api/branches` and
/// embedded under `SessionView::branch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchView {
    pub id: String,
    /// Short label: the branch name with the optional `weaver/` prefix stripped.
    pub name: String,
    pub title: String,
    pub goal: String,
    /// The agent's current-state message, set via `weaver status`, shown even
    /// when the branch is calm. The attention *level* is the `attention` tag.
    pub description: String,
    /// Every tag on the branch (the agent's `attention`, an overlooker's
    /// `triage`, and any free-form key), ordered by key. Empty when the branch is
    /// calm and unmarked — absence is the default state, there is no `ok` tag.
    pub tags: Vec<TagView>,
    pub repo_root: String,
    pub branch: String,
    pub base_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub open_issue_count: i64,
    /// The branch's latest GitHub pull-request snapshot (link, review decision,
    /// check rollup), or `null` when GitHub polling is off, the repo has no
    /// remote PR, or `gh` is unavailable. Maintained by the loom poll loop.
    pub github: Option<GithubStatus>,
}

impl BranchView {
    /// Build the wire view from a branch plus the parts the server gathered (its
    /// tags, the open-issue count, and the latest GitHub snapshot). The async DB
    /// lookups that produce those parts live in the loom server.
    pub fn from_parts(
        branch: &Branch,
        tags: &[Tag],
        open_issue_count: i64,
        github: Option<GithubStatus>,
    ) -> Self {
        let name = branch
            .branch
            .strip_prefix("weaver/")
            .unwrap_or(&branch.branch)
            .to_string();
        BranchView {
            id: branch.id.clone(),
            name,
            title: branch.title.clone(),
            goal: branch.goal.clone(),
            description: branch.description.clone(),
            tags: tags.iter().map(TagView::from).collect(),
            repo_root: branch.repo_root.clone(),
            branch: branch.branch.clone(),
            base_branch: branch.base_branch.clone(),
            created_at: branch.created_at.clone(),
            updated_at: branch.updated_at.clone(),
            open_issue_count,
            github,
        }
    }
}

/// Session-scoped view returned by the `/api/sessions[/...]` endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    pub status: String,
    pub work_dir: String,
    pub term_session: String,
    pub agent_kind: String,
    pub model: String,
    pub effort: String,
    pub github_repo: Option<String>,
    pub last_activity_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// Branch id of the session that **launched** this one — the parent in the
    /// dashboard's session tree — or `null` for a top-level session.
    pub parent_id: Option<String>,
    /// The tracking issue opened for this session's task at launch (the handle
    /// handed back to whoever launched it). Only populated on the create
    /// response; `None` on the list/get/patch paths, which don't recompute it.
    pub tracking_issue: Option<i64>,
    pub branch: BranchView,
}

/// Issue as the API exposes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueView {
    pub id: i64,
    pub repo_root: String,
    pub github_repo: Option<String>,
    /// Branch the issue was created from (provenance).
    pub source_branch: Option<String>,
    /// Branch currently working it; `null` is the unclaimed repo backlog.
    pub claimed_branch: Option<String>,
    pub title: String,
    pub body: String,
    pub status: String,
    pub github_issue: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// Free-form `(key, value)` labels on the issue, rendered as quiet pills.
    /// Empty when the issue carries none. Unlike branch tags these never carry
    /// the loud `attention`/`triage` ladder.
    pub tags: Vec<TagView>,
}

impl IssueView {
    /// Build the wire view from an [`Issue`] and the tags gathered for it.
    pub fn from_parts(i: Issue, tags: &[Tag]) -> Self {
        IssueView {
            id: i.id,
            repo_root: i.repo_root,
            github_repo: i.github_repo,
            source_branch: i.source_branch,
            claimed_branch: i.claimed_branch,
            title: i.title,
            body: i.body,
            status: i.status,
            github_issue: i.github_issue,
            created_at: i.created_at,
            updated_at: i.updated_at,
            closed_at: i.closed_at,
            tags: tags.iter().map(TagView::from).collect(),
        }
    }
}

impl From<Issue> for IssueView {
    /// Convenience for call sites that don't surface tags (the tag list is left
    /// empty). Tag-aware endpoints use [`IssueView::from_parts`].
    fn from(i: Issue) -> Self {
        IssueView::from_parts(i, &[])
    }
}

// ---------------------------------------------------------------------------
// Artifacts — named, versioned documents an agent (or the user) writes to
// weaver. The envelope, a version row, and the full view (content + projected
// references). The projection backs both the SPA chips and `weaver artifact
// show`. See docs/artifacts.md.
// ---------------------------------------------------------------------------

/// An artifact envelope as the API exposes it: identity, kind, title, scope, and
/// its latest revision number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub title: String,
    /// The branch that owns it, or `null` for a repo-shared artifact.
    pub branch_id: Option<String>,
    /// The latest revision number.
    pub rev: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One revision of an artifact (metadata only — the version picker lists these;
/// content is fetched per-rev through the artifact GET with `?rev=`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub rev: i64,
    /// `agent` | `user` — who wrote this revision.
    pub author: String,
    pub created_at: String,
}

/// The live status of one issue referenced from an artifact — what the renderer
/// stamps into a `#N` chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRefStatus {
    pub id: i64,
    pub title: String,
    /// `open` | `closed`.
    pub status: String,
    /// The branch working it; `null` is the unclaimed backlog.
    pub claimed_branch: Option<String>,
}

/// The projected reference map an artifact's content names. Keyed by id-as-string
/// so it round-trips cleanly through JSON object keys. v1 projects issues; the
/// `artifact:`/`session:` reference kinds are reserved for later probes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactRefs {
    /// `{"<issue id>": { id, title, status, claimed_branch }}` for every `#N`
    /// the content references.
    #[serde(default)]
    pub issues: std::collections::BTreeMap<String, IssueRefStatus>,
}

/// The full artifact view returned by the artifact GET/PUT: the envelope, the
/// content of the selected (default latest) revision, the version list for a
/// picker, and the projected reference map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactView {
    pub meta: ArtifactMeta,
    /// Raw content of the selected revision — the dashboard renders and edits it.
    pub content: String,
    /// Every revision, newest first, for the version picker.
    pub versions: Vec<ArtifactVersion>,
    /// References found in the content, resolved against the live ledger.
    pub refs: ArtifactRefs,
}

/// One overlooker, as the API exposes it. The JSON-bearing columns
/// (`trigger`, `scope`, `params`) are returned as **parsed** structured JSON so
/// a UI never re-parses strings; `capabilities` is a real array; the rest is the
/// stored definition plus its schedule bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlookerView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// The event-match predicate, parsed: `{cron|every|event|level|repo}`.
    pub trigger: Value,
    /// The fleet query a round surveys, parsed: `{attention?, repo?}`.
    pub scope: Value,
    /// `builtin:<name>` for a stock program, or an absolute path under
    /// `~/.weaver/overlookers/` for a custom one.
    pub program: String,
    /// Stock-program parameters (e.g. the judgement `prompt`), parsed.
    pub params: Value,
    /// The granted capability set (the intervention ladder). `observe` is
    /// implicit; the rest are explicit grants.
    pub capabilities: Vec<String>,
    pub model: String,
    pub effort: String,
    pub cooldown_secs: i64,
    /// Warm mode (`params.warm`): the engine keeps one long-lived, fleet-hidden
    /// session for this overlooker so it has across-round memory.
    pub warm: bool,
    /// The id of that warm session once the engine has created it, else `null`.
    /// Its live terminal is reachable from the overlooker's detail page (the
    /// session is hidden from the fleet listing).
    pub warm_session_id: Option<String>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    /// The most recent round's outcome (`ok|noop|skipped|error`), or `null` if
    /// it has never run — the at-a-glance health a list view shows.
    pub last_outcome: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl OverlookerView {
    /// Build the wire view from an overlooker plus the most recent round's
    /// outcome (the server reads that from the run history). The JSON columns
    /// are parsed here via the domain accessors.
    pub fn from_parts(o: &Overlooker, last_outcome: Option<String>) -> Self {
        Self {
            id: o.id.clone(),
            name: o.name.clone(),
            enabled: o.enabled,
            trigger: serde_json::to_value(o.trigger()).unwrap_or(Value::Null),
            scope: serde_json::to_value(o.scope()).unwrap_or(Value::Null),
            program: o.program.clone(),
            params: o.params(),
            capabilities: o.capabilities(),
            model: o.model.clone(),
            effort: o.effort.clone(),
            cooldown_secs: o.cooldown_secs,
            warm: o.warm(),
            warm_session_id: o.warm_session_id.clone(),
            last_run_at: o.last_run_at.clone(),
            next_run_at: o.next_run_at.clone(),
            last_outcome,
            created_at: o.created_at.clone(),
            updated_at: o.updated_at.clone(),
        }
    }
}

/// One round in an overlooker's history (the audit trail), with `actions`
/// parsed back into JSON for a UI to render. The `stdout`/`stderr`/`exit_code`/
/// `duration_ms` fields are the captured execution log — what the script printed
/// and returned — surfaced so a run page shows exactly what happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlookerRunView {
    pub id: i64,
    pub trigger_reason: String,
    /// The normalized event that woke the round (`cron` / `manual` / e.g.
    /// `pr.merged`).
    pub trigger_event: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
    pub summary: String,
    /// The JSON array of marks / nudges / would-dos the round recorded.
    pub actions: Value,
    /// A tail of the script's standard output.
    pub stdout: String,
    /// A tail of the script's standard error.
    pub stderr: String,
    /// The interpreter's exit status, or `null` when it never spawned / timed out.
    pub exit_code: Option<i64>,
    /// Wall-clock the program ran, in milliseconds.
    pub duration_ms: Option<i64>,
}

impl From<OverlookerRun> for OverlookerRunView {
    fn from(r: OverlookerRun) -> Self {
        Self {
            id: r.id,
            trigger_reason: r.trigger_reason,
            trigger_event: r.trigger_event,
            started_at: r.started_at,
            finished_at: r.finished_at,
            outcome: r.outcome,
            summary: r.summary,
            actions: serde_json::from_str(&r.actions).unwrap_or(Value::Null),
            stdout: r.stdout,
            stderr: r.stderr,
            exit_code: r.exit_code,
            duration_ms: r.duration_ms,
        }
    }
}

/// One **program** an overlooker can run, as `GET /api/overlookers/programs`
/// exposes it. Builtin programs are Python scripts that ship inside the loom
/// binary; the embedded source is returned for a read-only view in the panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramView {
    /// The reference an overlooker's `program` field names it by, e.g.
    /// `builtin:status` or `builtin:archive-merged`.
    pub program: String,
    pub title: String,
    pub description: String,
    /// The program's embedded Python source. Read-only — it ships with the
    /// binary.
    pub source: String,
    /// Suggested starting config for a new overlooker running this program:
    /// `{trigger, scope, params, capabilities}` — what a create form prefills.
    pub defaults: Value,
}

// ---------------------------------------------------------------------------
// Request payloads
// ---------------------------------------------------------------------------

/// One launch-time scratch file: a name plus its base64-encoded bytes. JSON
/// can't carry raw binary, so the UI reads each dropped file as base64.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScratchUpload {
    pub name: String,
    #[serde(default)]
    pub content_base64: String,
}

/// Body for `POST /api/sessions`: launch a new session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateReq {
    pub cwd: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub issue: Option<i64>,
    /// A pre-existing weaver issue id to claim for this session (fan-out
    /// pickup). Seeds title/goal/description and stamps `claimed_branch`.
    #[serde(default)]
    pub claim_issue: Option<i64>,
    #[serde(default)]
    pub existing_branch: Option<String>,
    /// The branch (id or name) of the agent launching this session, when it is
    /// itself a weaver session delegating work. Recorded as the tracking
    /// issue's `source_branch`. The `loom` CLI fills this from `$WEAVER_BRANCH`;
    /// a human/dashboard launch leaves it unset.
    #[serde(default)]
    pub parent_branch: Option<String>,
    /// Model tier ('haiku' | 'sonnet' | 'opus' | 'fable'); blank/absent
    /// inherits the configured `agent.claude_args`.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning effort ('low' | 'medium' | 'high' | 'xhigh' | 'max');
    /// blank/absent inherits the configured `agent.claude_args`.
    #[serde(default)]
    pub effort: Option<String>,
    /// Reference files to drop into the new worktree's `scratch/` directory
    /// before the agent launches. Empty/absent for a plain session.
    #[serde(default)]
    pub scratch: Vec<ScratchUpload>,
}

/// Body for `PATCH /api/sessions/{id}`. Branch-level fields (goal/title/
/// description) are forwarded to the underlying branch row. The attention *level*
/// is set through the tags endpoints (`PUT/DELETE /sessions/{id}/tags/{key}`),
/// not here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchSessionReq {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    /// The agent's current-state message — the prose shown beside the level.
    #[serde(default)]
    pub description: Option<String>,
}

/// Body for `PUT /api/sessions/{id}/tags/{key}`: set (upsert) a tag. The `key`
/// is the path segment; this carries the rest. For a loud key (`attention` |
/// `triage`) `value` is `attention` | `blocked` — to return to calm, `DELETE`
/// the tag rather than setting an `ok` value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagReq {
    pub value: String,
    /// One-line reason accompanying the tag.
    #[serde(default)]
    pub note: String,
    /// Who is setting it (an overlooker name or `manual`); the server defaults a
    /// missing author.
    #[serde(default)]
    pub by: Option<String>,
}

/// Body for `POST /api/sessions/{id}/send`: type a message into the agent pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendReq {
    /// The text to type into the agent's pane.
    pub text: String,
    /// Whether to follow the text with Enter to submit it (and so trigger an
    /// agent round). Defaults to true; pass false to stage input unsubmitted.
    #[serde(default = "default_submit")]
    pub submit: bool,
    /// Who is sending (an overlooker name or `manual`) — recorded on the
    /// `nudge` audit event; the server defaults a missing author.
    #[serde(default)]
    pub by: Option<String>,
}

fn default_submit() -> bool {
    true
}

impl SendReq {
    /// A submitting send (the default): type `text` and press Enter.
    pub fn submit(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            submit: true,
            by: None,
        }
    }
}

/// Body for `POST /api/agent/oneshot`: run a fresh, env-stripped one-shot
/// headless agent (`claude -p`) with `prompt` on stdin and return its stdout
/// as `{output}` (`null` when the agent is absent or fails — callers degrade
/// gracefully). The judgement primitive overlooker programs reach through the
/// daemon, which owns the agent command and timeout configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOneshotReq {
    pub prompt: String,
    /// Model tier override (`haiku` | `sonnet` | `opus` | `fable`); empty
    /// inherits the agent's default.
    #[serde(default)]
    pub model: String,
    /// Reasoning effort override; empty inherits.
    #[serde(default)]
    pub effort: String,
}

/// Body for `POST /api/branches/{id}/issues`: create an issue claimed by a
/// branch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateIssueReq {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub github_issue: Option<i64>,
}

/// Body for `PATCH /api/issues/{id}`: every mutable field optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchIssueReq {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// "open" or "closed".
    #[serde(default)]
    pub status: Option<String>,
}

/// Body for `POST /api/repos/issues`: create an unclaimed repo-level backlog
/// item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateRepoIssueReq {
    pub repo_root: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub github_issue: Option<i64>,
}

/// Body for `PUT /api/sessions/{id}/artifacts/{name}`: a user edit that appends
/// a new revision (`author: user`). `title`/`kind` update the envelope; omit
/// them to keep the current values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactWriteBody {
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Body for `POST /api/overlookers`. JSON-bearing fields take structured JSON
/// (`trigger`/`scope`/`params`), which the server serializes into the stored
/// text columns. Optional fields fall back to the model's defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateOverlookerReq {
    pub name: String,
    #[serde(default)]
    pub trigger: Option<Value>,
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<i64>,
}

/// Body for `PATCH /api/overlookers/{id}`: every mutable field optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchOverlookerReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub trigger: Option<Value>,
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<i64>,
}

/// Body for `POST /api/overlookers/{id}/run`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunOverlookerReq {
    #[serde(default)]
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// Which sign-in methods the server currently offers — what the login screen
/// renders. `password` is always available (any user can be given one);
/// `github` is true only once an OAuth app is configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethods {
    pub password: bool,
    pub github: bool,
}

/// `GET /api/auth/me` — who the caller is and what the login screen needs. The
/// SPA hits this on load: `authenticated: false` means show the login view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeView {
    pub authenticated: bool,
    /// The approved username, when authenticated.
    pub username: Option<String>,
    /// The caller's GitHub login, when known.
    pub github_login: Option<String>,
    /// How they authenticated: `loopback` | `token` | `session` | null.
    pub via: Option<String>,
    /// The sign-in methods on offer (for the login screen).
    pub methods: AuthMethods,
}

/// One API token's non-secret metadata (`GET /api/auth/tokens`). The secret
/// itself is only ever returned once, in [`CreatedTokenView`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenView {
    pub id: String,
    pub name: String,
    /// The non-secret leading slice, e.g. `loom_AbCd…`, to tell tokens apart.
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

/// `POST /api/auth/tokens` reply — the one and only time the plaintext token is
/// shown. Store it now; the server keeps only a hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedTokenView {
    /// The full secret — present once, never retrievable again.
    pub token: String,
    #[serde(flatten)]
    pub info: TokenView,
}

/// Body for `POST /api/auth/tokens`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateTokenReq {
    pub name: String,
    /// Optional lifetime in days; omitted / non-positive means it never expires.
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

/// Body for `POST /api/auth/login` (username/password).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

/// Body for `POST /api/auth/password` — set/change the caller's own password.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetPasswordReq {
    pub new_password: String,
}

/// One approved operator (`GET /api/auth/users`). The password hash is never
/// exposed — only whether one is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserView {
    pub username: String,
    pub github_login: Option<String>,
    pub has_password: bool,
    pub created_at: String,
}

/// Body for `POST /api/auth/users` — approve a new operator.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddUserReq {
    pub username: String,
    #[serde(default)]
    pub github_login: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

/// `GET /api/auth/github/config` — the GitHub sign-in setup, secret withheld.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubConfigView {
    /// Whether both a client id and secret are present (sign-in is live).
    pub configured: bool,
    /// The OAuth app's client id (public). Empty when unset.
    pub client_id: String,
    /// The callback path to register on the GitHub OAuth app
    /// (`/api/auth/github/callback`).
    pub callback_path: String,
}

/// Body for `PUT /api/auth/github/config`. The secret is write-only — send it to
/// set it, omit it to leave the stored one untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetGithubConfigReq {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
}
