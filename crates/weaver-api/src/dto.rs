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
    /// The agent's current-state message, set via `weaver set-status`, shown even
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
    pub tmux_session: String,
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
    /// Link to a plan task (`"<slug>#T3"`) when materialized from a plan.
    pub plan_task: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

impl From<Issue> for IssueView {
    fn from(i: Issue) -> Self {
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
            plan_task: i.plan_task,
            created_at: i.created_at,
            updated_at: i.updated_at,
            closed_at: i.closed_at,
        }
    }
}

/// One task in a plan, with status PROJECTED from the linked issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTaskView {
    pub id: String,
    pub title: String,
    pub exec: String,
    pub value: String,
    pub deps: Vec<String>,
    /// Linked issue (the materialization), if any — the projected state.
    pub issue_id: Option<i64>,
    pub issue_status: Option<String>,
    pub claimed_branch: Option<String>,
}

/// A session's structured project plan: design + task breakdown from a markdown
/// file, with each task's status joined from the issue ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanView {
    pub slug: String,
    /// Worktree-relative path, for the file-write (Edit) endpoint.
    pub path: String,
    pub title: String,
    pub status: String,
    /// Raw markdown source — the dashboard renders and edits this.
    pub content: String,
    pub tasks: Vec<PlanTaskView>,
    /// Every plan slug in the repo, for a picker.
    pub available: Vec<String>,
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
/// parsed back into JSON for a UI to render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlookerRunView {
    pub id: i64,
    pub trigger_reason: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
    pub summary: String,
    /// The JSON array of marks / nudges / would-dos the round recorded.
    pub actions: Value,
}

impl From<OverlookerRun> for OverlookerRunView {
    fn from(r: OverlookerRun) -> Self {
        Self {
            id: r.id,
            trigger_reason: r.trigger_reason,
            started_at: r.started_at,
            finished_at: r.finished_at,
            outcome: r.outcome,
            summary: r.summary,
            actions: serde_json::from_str(&r.actions).unwrap_or(Value::Null),
        }
    }
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
        }
    }
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
