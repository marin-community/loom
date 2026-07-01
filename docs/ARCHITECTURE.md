# Architecture

Deep reference for weaver's internals. [AGENTS.md](../AGENTS.md) is the short
how-to-work guide and links here; this file is for when you need the full map.

## Mental model

weaver ships **two binaries** over a **shared sqlite database**:

- **`weaver`** — the **agent-facing CLI**: database-direct (no daemon, no HTTP),
  resolving "the current branch" from `$WEAVER_BRANCH` else the git checkout
  under cwd. Cold start is sub-50ms; it carries no `axum` / `reqwest` / SPA
  dependencies. Agents call it to read and update the goal, report status, add
  issues, set tags, and emit hook events. It **works whether or not `loom` is
  running** — that decoupling is the point of the split.
- **`loom`** — the **optional orchestrator**: the REST + SSE server, the Vue web
  UI, the per-branch terminal supervisor + agent process (via the `sessions`
  table), the background monitor, and the `git worktree` shell-outs. Without loom,
  branches and issues still work; the terminal orchestration, the dashboard, and
  the live screen do not.

```
weaver CLI ──sqlite──┐
                     ├─ ~/.weaver/weaver.db   (shared, WAL)
loom server run ────┘    │
  │                       │
  ├─ axum REST + SSE      │
  ├─ terminal + git wrap. │
  ├─ agent launcher       │
  ├─ monitor (consumes    │
  │   `events` rows that  │
  │   `weaver hook` wrote)│
  └─ Vue SPA              │
                          │
  Vue SPA ──REST + SSE────┘
```

Both binaries open the sqlite file directly. The monitor watches the `events`
table for new `hook` rows — that is how `weaver hook` reports status without
needing the daemon to be reachable.

## Module layout

| Path | What's in it |
|---|---|
| `crates/weaver-core/` | lib: `branches`, `issues`, `events`, `db`, `migrations` (ordered SQL + `schema_migrations` indicator), `git`, `config`, `artifacts` (versioned documents), `repo_config` (`.weaver/config.toml`), `transcript` (agent conversation logs: raw → iris format → markdown), agent helpers. Pure logic; used by both binaries. |
| `crates/smartdoc/` | the markdown-convention layer: parse references (`#N`, `artifact:<name>`), project live status into the render. Dependency-free of weaver. See [artifacts.md](artifacts.md). |
| `crates/weaver/src/bin/weaver.rs` | the slim agent-facing CLI (`goal`, `summary`, `readme`, `status` [read or set level + message], `tag` [`set`/`rm`/`ls` a branch tag], `issue …`, `where`, `log`, `chatlog` [render the agent's conversation transcript], `hook`, `config`) |
| `crates/loom/src/web.rs` | axum routes, request/response types, SSE — **the API surface** (incl. the auth middleware + login/token/user handlers) |
| `crates/loom/src/auth.rs` | authentication core: token/password crypto, the `users`/`api_tokens`/`auth_sessions` tables, the machine-local token, and the GitHub OAuth calls. `axum`-free so it unit-tests directly |
| `crates/loom/src/server.rs` | bind, write `server.json`, spawn bg tasks |
| `crates/loom/src/monitor.rs` | status detection, orphan marking, hook-event consumer |
| `crates/loom/src/overlooker.rs` | the overlooker engine: cron timer + event dispatcher + the round executor (the script subprocess executor every program runs on) |
| `crates/loom/src/builtins.rs` | the builtin overlooker program registry; the script programs are real Python files in `crates/loom/overlookers/`, embedded into the binary |
| `python/weaver-loom/` | the pure-Python layer over the loom REST API (`weaver_loom`: client + overlooker round context); stdlib-only, uv-buildable, vendored onto every script's `PYTHONPATH` by the engine; server-free contract tests in `tests/` (`uv run pytest`, CI's `python-binding` job) |
| `crates/loom/src/agent.rs` | launching agents into per-session terminals + installing `.claude/settings.local.json` hooks + the one-shot headless agent behind `POST /api/agent/oneshot` |
| `crates/loom/src/session.rs` | `Session` row + sqlx queries |
| `crates/loom/src/chatlog.rs` | conversation log: capture at archive (write the iris `chat.json` + rendered `chat.md` under `session.log_dir`) and serve it for the Conversation tab (`conversation()` — live transcript, else the capture) |
| `crates/loom/src/backend.rs` | the terminal-management seam: every programmatic terminal op (create/has/capture/send/kill/list) drives the session's `tapestry` supervisor |
| `crates/tapestry/` | the terminal backend: a per-session detached PTY supervisor (PTY + vt100 screen emulator + unix control socket) that outlives loom and streams raw PTY bytes, so an attached xterm owns its own scrollback/search |
| `crates/loom/src/terminal.rs` | WebSocket ⇄ live-terminal bridge: xterm.js ⇄ the tapestry session socket |
| `crates/loom/src/github.rs` | `gh` CLI shell-out: issue seeding, PR opening, and the PR-status poll loop (snapshots each branch's PR; archives on merge) |
| `crates/loom/src/client.rs` | HTTP client used by the `loom` CLI to talk to its own daemon |
| `crates/loom/src/bin/loom.rs` | the orchestrator CLI (`server`, `session`, `ps`, `attach`, …) |
| `crates/loom/frontend/` | Vue 3 SPA, rspack, Tailwind. `api.ts` + views in `views/`; the visual rules live in [loom-ui.md](loom-ui.md) |
| `crates/loom/static/dist/` | Build output (placeholder; real build overwrites) |
| `crates/loom/tests/` | integration tests: `integration/` (server suites) + `hook_monitor.rs`; need `git` (they spawn `tapestry` supervisors, built by the same `cargo test`) |
| `e2e/` | Playwright; talks to a real `loom server run`. Separate `package.json` |
| `crates/loom/build.rs` | Builds the SPA into `static/dist` (npm + rspack); writes a placeholder when Node is unavailable |

## Build internals

`cargo build` builds the SPA into `static/dist` via `build.rs`; loom serves it
from there at runtime (`web::static_dir`). `rerun-if-changed` makes the SPA build
a no-op when no frontend source changed, so backend-only edits don't re-run
rspack; a Node-less checkout still builds (the backend) and serves a placeholder.
There is no skip flag — backend and frontend are separated at the **test** level
(`cargo test` for the backend, the Playwright `e2e/` suite for the frontend), not
the build level.

The integration tests shell out to real `git` and spawn `tapestry` terminal
supervisors (detached PTY processes). The harness kills its supervisors on drop;
if one hangs, look for stray `tapestry supervise` processes.

### Agent lint review

`scripts/lint-review.py` — a self-contained `uv run` script — catches the *agent
slop* fmt and clippy can't: the judgement calls of naming, API shape,
dead/speculative code, duplication, and comment/test quality. It builds one
prompt from the [`docs/lint.md`](lint.md) catalog (`wl-...` rules) plus the diff
and pipes it to a headless `claude -p` sub-agent, run as a fresh session — the
calling session's `CLAUDE_CODE_*` / `ANTHROPIC_API_KEY` markers are stripped so
it neither nests in the caller's transcript nor bills the metered API. It parses
the findings and **errors on any at or above a confidence threshold**; the rest
print as advisory.

It is **not** wired into the pre-commit hook. `scripts/pre-commit.sh` stays a
fast fmt + clippy gate identical to CI; the lint review is a separate, explicit
step in the commit → PR flow — agents run it via the `pull-request` skill after
committing and before opening the PR. Keeping the agent out of the commit path
means a slow or flaky review never wedges a commit. Run `scripts/lint-review.py`
to review the whole branch against its merge-base with `main`.

It **self-skips** (exit 0) when `claude` isn't on PATH, when there are no
Rust/TS/Vue changes, or when the agent times out or errors — so a flaky or
absent agent can't block progress, and only real findings do.

Knobs: `WEAVER_SKIP_AGENT_LINT=1` to skip a run, `WEAVER_LINT_MIN_CONFIDENCE`
(default `0.9`), `WEAVER_LINT_AGENT_CMD` (default `claude -p`), and
`WEAVER_LINT_TIMEOUT` (default `600`s). Suppress a false positive with a trailing
`// wl-allow: <code>` on the cited line.

### End-to-end (Playwright)

The `e2e/` suite drives the real UI against a real server. It boots **one**
`loom server run` per Playwright *worker* (not per test) on a random port, each with
its own `WEAVER_HOME` / sqlite db (which also scopes the `tapestry` terminal
sockets) and a throwaway git repo (see `e2e/fixtures/weaver.ts`),
using the deterministic `shell` agent. The per-test `weaver` fixture wipes every
session (branch + worktree) between tests, so each starts from a clean slate and
count-based assertions hold regardless of order. Workers are fully isolated, so
the suite runs in parallel (`fullyParallel`, `workers > 1`) and — because every
session it touches is scoped to a worker's private socket and db — can't disturb
a long-running dev server or your `~/.weaver` sessions. A `globalSetup` runs
`cargo build` once up front so workers never race on the build.

```sh
cd e2e
npm install            # first run only; also fetches the browser (see below)
npx playwright install chromium
npm test               # runs the suite; rebuilds loom + the SPA if stale
```

On a Linux distro Playwright doesn't ship a prebuilt browser for (e.g.
`ubuntu26.04`, where `playwright install` errors with "does not support
chromium"), force the nearest supported fallback build with
`PLAYWRIGHT_HOST_PLATFORM_OVERRIDE`, and set it for the test run too so the same
binary is launched:

```sh
PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04-x64 npx playwright install chromium
PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04-x64 npm test
```

## Storage & state

- **SQLite** at `$WEAVER_HOME/weaver.db` (default `~/.weaver/weaver.db`),
  shared by `weaver` and `loom`. WAL mode handles concurrency.
  - Core tables: `branches`, `issues`, `events`, `settings`.
  - Loom tables (`crates/loom/src/db.rs`): `sessions`, `recent_repos`,
    `branch_github` (per-branch PR snapshot), and the auth tables `users`
    (the approved-operator allowlist, seeded with the owner), `api_tokens`
    (hashed bearer tokens), and `auth_sessions` (hashed login cookies). See
    [Authentication](#authentication).
  - **Schema migrations** (`weaver-core/src/migrations.rs`): ordered SQL files
    under `crates/weaver-core/migrations/` (`NNNN_name.sql`, embedded with
    `include_str!`), applied at startup and recorded in a `schema_migrations`
    indicator table so each runs once. Add a change as a new numbered file plus
    a row in `MIGRATIONS`; never edit one that has shipped. A pre-framework
    database is brought to the baseline by a one-time `legacy_bootstrap` on
    first run.
- **`server.json`** in `$WEAVER_HOME`: pid + bound addr, written when `loom`
  comes up. The `loom` CLI uses it to find the daemon when `WEAVER_API` is
  unset.
- **Settings** live in the `settings` table; each key is declared in
  `weaver-core::config::registry()`. Both binaries read it. This is the
  **global** (machine/user) store; **per-repo** conventions instead live in a
  committed `.weaver/config.toml` read by `weaver-core::repo_config` — distinct
  from the settings table, and resolved repo-file → builtin-default like a repo's
  own `WEAVER.md`.
- **Worktrees** live under `<repo>/.worktrees/<slug>` on `weaver/<slug>`
  (unless `--branch` reused an existing branch).

## REST API

All routes live under `/api`. The Vue SPA is the primary consumer.

| Method + path | What it does |
|---|---|
| `GET /api/health` | liveness probe |
| `GET /api/sessions` / `POST /api/sessions` | list / create sessions (create takes optional `scratch: [{name, content_base64}]` and `parent_branch`; opens a tracking issue and returns its id as `tracking_issue`) |
| `GET PATCH DELETE /api/sessions/{id}` | session CRUD (status, title, goal, description) |
| `PUT DELETE /api/sessions/{id}/tags/{key}` | set (upsert) / clear a branch tag — the well-known `attention` and `triage` keys plus any free-form key |
| `POST /api/sessions/{id}/{archive,adopt}` | actions |
| `POST /api/sessions/{id}/github` | re-poll the branch's GitHub PR now and return the updated session |
| `GET POST DELETE /api/sessions/{id}/scratch` | list / drop / remove worktree `scratch/` reference files |
| `PUT /api/sessions/{id}/file?path=…` | write raw bytes to a worktree file (the editor save primitive) |
| `GET /api/sessions/{id}/artifacts` | list the branch's [artifacts](artifacts.md) plus repo-shared ones |
| `GET PUT /api/sessions/{id}/artifacts/{name}` | read content + projected refs (`rev=N` for a revision) / write a user edit as a new revision |
| `GET /api/sessions/{id}/{diff,log,events}` | reads + SSE stream |
| `GET /api/sessions/{id}/conversation` | the agent conversation as a normalized iris log (live transcript, else the archive capture); 404 when there is none — backs the Conversation tab |
| `GET /api/sessions/{id}/terminal` | WebSocket: xterm.js ⇄ the session's tapestry PTY (the interaction surface) |
| `POST /api/sessions/{id}/send` | type `{text}` into the agent's terminal; `submit` (default true) follows it with Enter to trigger a round |
| `POST /api/sessions/{id}/interrupt` | send a break (Escape) to the terminal — stop the current turn |
| `GET /api/sessions/{id}/preview?lines=N` | capture the screen as `{screen}`; `lines` adds scrollback above the visible screen |
| `GET /api/branches` / `GET PATCH /api/branches/{id}` | list / inspect / edit tracked branches |
| `GET POST /api/branches/{id}/issues` | issues claimed by the branch / create one |
| `GET /api/issues?all=…` | the cross-repo issue board (every repo's issues; `all=true` includes closed) — what the loom Issues pane reads |
| `GET PATCH DELETE /api/issues/{id}` | per-issue CRUD |
| `PUT DELETE /api/issues/{id}/tags/{key}` | set (upsert) / clear a free-form issue label — quiet `(key, value)` pills, no loud `attention`/`triage` ladder |
| `GET POST /api/repos/issues?repo_root=…` | repo-wide board (`scope=repo\|backlog`) / create a backlog item |
| `GET /api/repos/recent` / `GET /api/repos/branches?cwd=…` | recent repos / branches in a repo |
| `GET /api/agents` | first-class agent types and their advertised model/effort selectors — backs the create-session form and server-side validation |
| `GET PATCH /api/settings` | settings registry |
| `GET /api/auth/me` | caller identity + sign-in methods (public; never 401s) |
| `POST /api/auth/login` / `POST /api/auth/logout` | username/password login / drop the session (public) |
| `GET /api/auth/github/{login,callback}` | the GitHub OAuth dance (public) |
| `GET POST /api/auth/tokens` / `DELETE /api/auth/tokens/{id}` | list / mint / revoke API tokens |
| `POST /api/auth/password` | set the caller's own password |
| `GET POST /api/auth/users` / `DELETE /api/auth/users/{username}` | the approved-operator allowlist |
| `GET PUT /api/auth/github/config` | the GitHub OAuth app config (secret write-only) |
| `GET POST /api/overlookers` / `GET PATCH DELETE /api/overlookers/{id}` | overlooker CRUD (see [Overlookers](#overlookers)) |
| `GET /api/overlookers/programs` | the builtin program registry: titles, suggested defaults, read-only script sources |
| `POST /api/overlookers/{id}/run` / `GET /api/overlookers/{id}/runs` | fire a round now (`{dry_run}` stubs mutations) / the round-history audit |

`SessionView` (`/api/sessions[/...]`) returns session-specific fields
top-level (`id`, `status`, `work_dir`, `term_session`, `agent_kind`, `model`,
`effort`, `pending_prompt`, `github_repo`, `last_activity_at`,
`created_at`, `updated_at`, `parent_id`, and — on the create response only —
`tracking_issue`) plus a nested `branch: BranchView`
(`id`, `name`, `title`, `goal`, `description`, `tags`,
`repo_root`, `branch`, `base_branch`, `created_at`, `updated_at`,
`open_issue_count`, `github`).

`BranchView::tags` is the branch's tag list — each a `TagView`
(`key`, `value`, `note`, `set_by`, `set_at`). A tag is a single-valued
`(key, value)` annotation on a branch; the well-known keys are `attention` (the
agent's self-report) and `triage` (an overlooker's assessment), and any other
key is a free-form, quiet pill. Absence of a key is the calm/default state —
there is no stored `ok` value; the list is empty for an unmarked branch. The
signal is **value-driven**, with a ladder on either side of calm: a value on the
attention ladder (`attention`/`blocked`) raises the branch on the dashboard
whatever its key, while a value on the *parked* ladder (`review` — the review
watch's `awaiting: review` mark) sinks it *below* the calm default in the fleet
sort, the quiet "waiting on an external actor, nothing for the user to do" end of
the spectrum (`weaver_core::tags::{ATTENTION_VALUES, PARKED_VALUES}`).

`SessionView::parent_id` is the branch id of the session that **launched** this
one — the parent in loom's session tree — or `null` for a top-level session. It
is stamped onto the `sessions` row at create time from the resolved
`parent_branch` (so reads need no extra query and the link can't drift), and is
`null` too when that parent is later untracked. The dashboard's session list
groups sessions into threads by it (children under their launcher, siblings by
launch time); a flat fleet with no sub-sessions is unchanged.

`BranchView::github` is the branch's latest GitHub pull-request snapshot
(`pr_number`, `pr_url`, `pr_state`, `pr_title`, `is_draft`, `review_decision`,
`checks`, `mergeable`, `merged_at`, `fetched_at`), or `null` when GitHub polling
is off, there is no PR, or `gh` is unavailable. See [GitHub
integration](#github-integration).

Status is two orthogonal axes. The session's `status` is the **lifecycle**
(orchestrator-owned, mechanical): `created` / `launching` / `running` /
`orphaned` / `done` / `error`. The branch's **`attention` tag** (value
`attention` | `blocked`, absent ⇒ calm) plus its `description` (a one-line
current-state message) are the **agent-declared** "does this need me?" signal,
both set via `weaver status`. The dashboard resolves and filters on the
attention signal.

There is **no** `/api/hook` endpoint — see [Status & tags](#status--tags).

**Scratch files** are reference material dropped into the worktree's `scratch/`
directory (git-ignored, so it never enters the agent's diff). They can be added
to a live session via `POST /api/sessions/{id}/scratch`, or attached up-front in
the New Session form: those ride in the create request as `scratch` and are
written *before* the agent launches, with a note appended to the launch prompt
so a fresh agent knows the files are there. The stored branch goal stays the
clean text the user typed.

**Launch base.** A new session's worktree forks from `base`. When the create
request omits it, `git::default_base` resolves the repo's default branch on
`origin` and fetches it, so the branch starts from a fresh `origin/<default>`
rather than the launching checkout's current branch. A remote-less repo (no
`origin`) degrades to the local current branch. The caller — the CLI's `--base`
or the create form's base field — can pin any ref instead.

**Driving the terminal.** `send` / `interrupt` / `preview` are one-shot HTTP
primitives over the supervisor's control socket (see `backend::send_literal`,
`send_key`, `capture`), distinct from the interactive terminal WebSocket: they
let an agent or script type into, interrupt, or read back a child session
uniformly. Each requires a live terminal (else 409). The CLI's `loom session
{send,break,preview}` wrap them.

## Runtime conventions

- **API-first.** New features land as a REST endpoint in `web.rs` first; the
  SPA and the `loom` CLI both consume it. Don't put business logic in
  `bin/loom.rs` or in the Vue layer.
- **Errors:** the server returns `AppError` (status + message + optional
  `details` map of per-field reasons); the `loom` CLI uses `anyhow` and prints
  `error: {e:#}`.
- **Async:** tokio everywhere on the server side. Long-running subprocesses
  (the terminal supervisor, git, gh, the agent) go through
  `tokio::process::Command`. The
  `weaver` CLI is synchronous-feeling (just a few `sqlx` calls per command).
- **Events:** state changes flow through `EventBus`; the SSE handler in
  `web.rs` fans them out. `weaver hook` writes directly to the `events`
  table, and loom's monitor tick promotes the new row into a session status
  change and a fresh `EventBus` notification.
- **No tracking-branch state in the server:** loom can be killed and restarted
  at any time. Terminal supervisors and worktrees survive (the supervisor is a
  detached process, independent of `loom server run`); "orphaned" is a first-class
  status, recovered via `loom session adopt` (or the Adopt button in the UI).

## Status & tags

Two distinct axes (see the SessionView note above): the mechanical **lifecycle**
`sessions.status`, and the agent-declared **attention** carried as a tag on the
branch.

**Tags** are single-valued `(key, value)` annotations on a branch, each with a
`note`, `set_by`, and `set_at`, stored in the shared `tags` table (one row per
`(branch_id, key)`, registry in [`weaver_core::tags`](../crates/weaver-core/WEAVER.md)).
**Loudness lives in the value, not the key:** a tag whose value is on the
`attention` | `blocked` ladder is *loud* (raises a badge) regardless of key, so
agents and watches both add loud tags without a privileged key registry. A tag's
**key is its type** (the chip label — `attention`, `review`, `stuck`, …) and its
**value is the severity**; every other value is a free-form, quiet pill. The
agent authors the well-known **`attention`** key for its own self-report; a watch
authors its own typed keys. The well-known **`idle`** key is a *quiet* exception:
loom stamps it mechanically when an agent goes quiet (the soothing "resting, no
one needed" state), carrying the non-loud value `idle` so it never raises a badge
— the dashboard surfaces it as a calm "Idle" mark, and the status watch may
replace it with a real loud status. Unlike a loud outside mark it is *not* subject
to activity-staleness (below): it is the agent's own lifecycle mark, cleared
event-driven by the next `working` hook (a submitted prompt), not retired by
`last_activity_at` advancing — the turn-ending output that fires the idle hook is
itself a pane change that bumps `last_activity_at`, so a stale-check would retire
the mark the instant it lands. **Absence is the calm/default state** — there
is no stored `ok`; returning to calm *clears* the tag. A tag is **stale** when its
`set_at` predates the session's `last_activity_at` (the session moved on since it
was set). The dashboard resolves the loudest non-stale loud tag into one
attention signal, with attribution (the agent's own, or an outside mark). The
agent's own `attention` self-report stays the *server-side* signal — what
`weaver status`, `resolve_attention`, and `weaver issue wait` read — so a watch's
outside marks surface on the dashboard without spuriously waking sub-agent
tracking.

**Lifecycle** is driven by the selected agent protocol. Claude Code currently
provides lifecycle hooks, so its protocol merges a `hooks` block into the
worktree's `.claude/settings.local.json` (see `loom::agent::install_hooks` and
`weaver_core::agent::hooks_json`); hookless protocols such as Codex and `shell`
start `running` immediately:

| Claude hook event | shells out to |
|---|---|
| `SessionStart` | `weaver hook --event session-start` (also injects `additionalContext`: the repo's `WEAVER.md`, or the builtin [crates/weaver-core/WEAVER.md](../crates/weaver-core/WEAVER.md), on a genuine start/resume/clear; after a **compaction** — `source: "compact"` on the hook's stdin — a concise `weaver summary` re-orientation instead, so the agent isn't re-fed the whole guide. `weaver readme` pulls the full guide back on demand) |
| `UserPromptSubmit` | `weaver hook --event working` |
| `Notification` | `weaver hook --event waiting` |
| `Stop` | `weaver hook --event idle` |

`weaver hook` writes an `events` row keyed on the branch resolved from
`$WEAVER_BRANCH` (set by the launcher) — no HTTP. Loom's monitor (`apply_hook`)
consumes new `hook` rows on its next tick. A `working` / `waiting` / `idle` hook
means the agent process is alive, so each sets `status = running` (this also
promotes a freshly `launching` session); `session-start` is recorded for the
primer injection but the launch path owns the initial status, so it drives no
liveness here. Liveness is all a work hook proves, so that is all the
orchestrator tracks — it does not infer working/waiting/idle from stillness.

The hooks also stamp a soothing, **quiet `idle` tag** — the calm "resting, no one
needed" state, deliberately *not* on the loud ladder, so an idle agent never
reads as needing the user. `working` (a prompt was submitted — the user is
engaged) returns the agent to calm, clearing both the `idle` mark and the agent's
own `attention` tag. `waiting` (a `Notification` lull) and `idle` (a turn ending)
both stamp the `idle` mark; they leave the agent's `attention` tag untouched, so a
loud self-report still wins the badge. We don't try to mechanically separate
"truly idle" from "waiting on a sub-agent or shell" — the finished-turn hook is a
good-enough idle signal, and the status watch upgrades it when warranted (below).

The **`attention` tag** is otherwise the agent's own call, set via `weaver
status <level> ["<message>"]`. That writes the tag (and, when a message is
given, the `description`) directly (daemon-less) — `ok` clears the tag, the two
loud levels upsert it — and records a `tag` event the monitor re-broadcasts over
SSE. A bare `weaver status <level>` changes only the level and keeps the last
message. Last write wins, so an explicit declaration overrides the hook-inferred
default. The general `weaver tag set|rm|ls` group writes any key the same way;
the `PUT`/`DELETE /api/sessions/{id}/tags/{key}` routes do it over HTTP for the
UI and the [overlooker](plans/overlooker.md). The builtin status watch, when a
session goes idle (the agent's finished-turn hook), asks the judge model for the
set of tags the session warrants and reconciles its own typed marks to that set
— never mirroring the agent's own `attention`. When the judge names a genuine
need, that session is actively waiting, not resting, so the watch *replaces* the
soothing `idle` mark with the real loud status; a "nothing needed" verdict leaves
`idle` in place.

Archiving a session clears its loud tags **and** the soothing `idle` mark: the
agent is gone, so a torn-down workstream can't still "need me" nor is it
"resting", and the dashboard stops flagging or labelling it. The UI also treats
any `archived` session as calm regardless of a stale tag left on the branch.

Archiving also **captures the agent's conversation log** (`crate::chatlog`,
inside the shared `web::archive`, so both the Archive button and the
merge-archive poller get it). The agent's transcript lives outside the worktree —
Claude Code under `~/.claude/projects/<munged-cwd>/`, Codex under
`~/.codex/sessions/` — so it survives the worktree removal; capture locates it,
normalizes it through `weaver_core::transcript` (raw → **iris format** → a
rendered markdown log), and writes `chat.json` (iris) + `chat.md` under
`<session.log_dir>/<branch>/` (`session.log_dir` defaults to
`~/.iris/logs/sessions`). It is best-effort: a missing or unreadable transcript
is a logged warning, never a failed archive. The same conversion/render pipeline
backs `weaver chatlog`, which renders the current worktree's (or a named file's)
transcript on demand.

The dashboard surfaces this as a **Conversation tab** on the session detail,
backed by `GET /api/sessions/{id}/conversation` (`chatlog::conversation` →
the live transcript when present, else the archived `chat.json`). The Vue viewer
renders the iris log natively — user/assistant turns, collapsible thinking, and
each tool call with its result — so a session stays reviewable in the UI after
its terminal is gone. While the agent is still live the tab is also drivable: a
composer at its foot sends a new prompt straight to the agent pane via `POST
/api/sessions/{id}/send` (type + Enter), and the log auto-refreshes on the
agent's lifecycle edges (the `status`/`tag` SSE events that fire at each
turn boundary), so a reply lands without a manual reload. The composer hides
once the terminal is gone (orphaned/done/archived), leaving the read-only log.

Orphan detection is independent: if the session's supervisor is no longer alive,
the session becomes `orphaned` and is eligible for `loom adopt`.

## GitHub integration

When the `gh` CLI is installed and authenticated, loom keeps a per-branch
pull-request snapshot alongside the session. A second background loop
(`github::poll`, sibling of the monitor, spawned in `server::serve`) ticks every
30s and, for each active session, runs `gh pr view <branch> --json …` from the
repo root. The result — PR number, URL, state (`OPEN`/`CLOSED`/`MERGED`), draft
flag, `reviewDecision`, a rolled-up `checks` verdict (`passing`/`failing`/
`pending`), and mergeability — is written to the loom-owned `branch_github`
table (one row per branch, keyed `branch_id`) and served as `BranchView.github`.
The dashboard renders it on the session list and overview; `POST
/api/sessions/{id}/github` forces an immediate re-poll.

The loop self-gates and degrades quietly: it is always spawned but does nothing
while the `github.poll` setting is off, `gh` is missing (probed once, cached via
`gh_available`), or the repo has no GitHub remote (a per-branch `gh` error that
is logged at debug and skipped). So it is a no-op on non-GitHub repos rather
than a failure.

**Archive on merge.** When a poll finds a branch's PR has merged and
`github.archive_on_merge` is on (the default), loom archives the session
automatically — the same teardown as the Archive button (`web::archive`, shared
code): the terminal killed, worktree removed, branch and weaver history kept. The
worktree is removed with `--force`, so any uncommitted work in it is discarded;
a merged PR is taken to mean the workstream is done. Turn the behaviour off with
`weaver config set github.archive_on_merge false` (or in the settings pane).
Both settings live in `weaver-core::config::registry()` under the **GitHub**
group.

`gh`-touching logic lives in `crate::github`: `fetch_pr` (the shell-out +
JSON parse + check rollup), `refresh` (fetch → store → announce → maybe
archive, behind both the poller and the refresh endpoint), and `poll` (the
loop). The merge-archive decision is split into `apply_snapshot` so it is
testable without invoking `gh`.

## Authentication

Authentication is a **loom-only** concern — the daemon-less `weaver` CLI talks
straight to sqlite and never authenticates. It lets loom be exposed off the
loopback interface (so the dashboard and the API are reachable without an SSH
tunnel) while gating who may drive the fleet. The core (crypto, the tables, the
GitHub OAuth calls) lives in `crate::auth`, deliberately `axum`-free; the HTTP
glue (the middleware, cookie handling, route handlers) lives in `crate::web`.

Every `/api` route except the public login surface (`/api/health`,
`/api/auth/me`, `/api/auth/login`, `/api/auth/logout`, `/api/auth/github/*`) and
the static SPA passes through the `require_auth` middleware, which resolves the
request to a `Principal` three ways, in order:

- **API token** — an `Authorization: Bearer loom_…` header. This is the
  `LOOM_TOKEN` a CI job or a remote `loom` CLI presents. Tokens are random
  secrets stored only as a SHA-256 hash (`api_tokens.token_hash`); the plaintext
  is shown once at creation. Managed under Settings → Tokens or `loom token`.
- **Session cookie** — the opaque `loom_session` cookie set by a successful
  GitHub or username/password login, stored hashed in `auth_sessions`.
- **Loopback trust** — a request from `127.0.0.1`/`::1` is taken to be the
  machine owner (the seeded primary user), gated on the `auth.trust_loopback`
  setting (on by default). This keeps the local CLI, the agent's in-worktree
  `loom` calls, and overlooker scripts working with zero configuration. To get
  the peer address, the server runs `into_make_service_with_connect_info`; the
  decision uses the real socket peer, **never** a forwarded header.

A request that resolves to none of these gets `401`; the SPA's router guard
turns that into the login screen.

**The allowlist.** `users` rows are the approved operators. A fresh database is
seeded with one owner — `github_login = rjpower` by default, overridable at
first run with `LOOM_OWNER_GITHUB`. GitHub login only succeeds for a login that
matches a `users` row; an unknown identity is authenticated by GitHub but
rejected by loom. A user may have a `github_login`, a `password_hash` (argon2),
or both. All approved users are equal — there is no role hierarchy, matching the
single-operator scale.

**GitHub OAuth** is configured per-deploy: register an OAuth app and set its id
and secret via Settings → Account or the `LOOM_GITHUB_CLIENT_ID` /
`LOOM_GITHUB_CLIENT_SECRET` env vars. The callback is
`<base>/api/auth/github/callback`, where `<base>` is the `auth.base_url` setting
or, unset, `{X-Forwarded-Proto|http}://{Host}`. The login route sets a short
CSRF `state` cookie the callback verifies. Until an app is configured the GitHub
button is hidden and `GET /api/auth/me` reports `methods.github = false`.

**The machine-local token.** On startup loom mints (and persists, 0600, at
`$WEAVER_HOME/loom-token`) a `kind = 'local'` `api_tokens` row owned by the
primary user, and injects it as `LOOM_TOKEN` into the environments of its own
same-host subprocesses (the agent's terminal, overlooker scripts) — and the `loom`
CLI reads it. This makes `auth.trust_loopback = false` a fully working mode:
behind a **same-host reverse proxy** (where forwarded requests look like
loopback and so trust must be off) local automation still authenticates via this
token, while remote callers must present their own. The local token is hidden
from the token list and is not revocable from the UI.

**Cookies** are `HttpOnly; SameSite=Lax; Path=/`; the `Secure` attribute is
added when `auth.cookie_secure` is on (set it when loom is reached over HTTPS).
loom terminates no TLS itself — run it behind a TLS-terminating proxy for remote
use. The `auth.*` settings live in `weaver-core::config::registry()` under the
**Authentication** group; the GitHub client id/secret are stored outside the
registry so the secret never rides `GET /api/settings`.

## Overlookers

An **overlooker** is a periodic / triggered watch program over the fleet: it
wakes on a trigger (a cron tick or a session event), surveys the sessions in
scope, and acts within an explicit capability set. The design of record is
[docs/plans/overlooker.md](plans/overlooker.md). The engine
(`loom::overlooker`, spawned in `server::serve`, self-gated on the
`overlooker.enabled` setting) runs each **round** under non-optional guardrails
— no-overlap, cooldown, a wall-clock timeout, no-recursion — and records it in
`overlooker_runs`, the audit trail the panel's round history renders.

A round runs the **program** the overlooker names:

- **Builtin scripts** — real Python files in `crates/loom/overlookers/`,
  embedded into the binary and registered in `loom::builtins`:
  `builtin:status` (stamp a `triage` mark on each in-scope session, judging
  via the configured `prompt` through the daemon's one-shot agent when
  available, else mirroring the agent's own `attention` tag),
  `builtin:review-wait` (park a session whose open, non-draft PR awaits an
  external review — `review_decision` `REVIEW_REQUIRED` — under a quiet
  `awaiting: review` mark that sinks it below the calm default in the fleet
  sort, and clear it once review lands, the PR merges, or it un-drafts; needs
  `mark`), `builtin:pr-label` (flag sessions whose open PR lacks the loom label)
  and `builtin:archive-merged` (flag live sessions whose PR has merged). The
  last two are **read-only**: they record `would:` actions and mutate nothing —
  the actual archive is still `github.archive_on_merge`, above. The Overlooker
  panel and `loom overlooker programs` list the registry; script sources
  render read-only (they ship with the binary).
- **A custom program file** — an absolute path, conventionally
  `~/.weaver/overlookers/<name>.py` (`loom overlooker new` scaffolds one).

Builtin scripts and custom files run on one executor: an env-stripped
subprocess that reaches the fleet only through the loom REST API — everything
loom can do is an HTTP route (including one-shot agent judgement, at
`POST /api/agent/oneshot`), and Python is purely a convenience layer on top.
There is deliberately no privileged in-Rust program shape: a builtin sees
exactly the API a custom program sees.
The contract: `$WEAVER_API` carries the daemon's base URL, `$WEAVER_OVERLOOKER`
the round's config (`{id, name, program, params, scope, capabilities, model,
effort, dry_run}`), and the script prints one JSON object — `{outcome, summary,
actions}` — as its final stdout line. A non-zero exit, no result object, or a
blown round budget records the round as an `error`. A mutating program must
honor `dry_run` (record `{would: …}` actions instead of acting) and stay inside
its granted capabilities.

That convenience layer is **`weaver_loom`** (`python/weaver-loom/`, stdlib-only):
a capability-gated `Client` over the REST routes plus the `Round` context
(config, scope-filtered survey, action log, result emission). The engine vendors
the module onto every script's `PYTHONPATH`, so programs import it with no
install step; for standalone iteration it installs with
`uv pip install -e python/weaver-loom`. The interpreter is `python3`, or
`uv run --script` when the script declares PEP 723 inline metadata and `uv` is
installed — so a custom program can declare third-party dependencies (the
builtins are stdlib-only and need neither).

## Environment

| Var | Purpose | Default |
|---|---|---|
| `WEAVER_HOME` | state directory | `~/.weaver` |
| `WEAVER_DB` | sqlite path | `$WEAVER_HOME/weaver.db` |
| `WEAVER_API` | loom URL (both sides — server binds, CLI talks) | `http://127.0.0.1:7878` |
| `WEAVER_BRANCH` | override the branch resolver (set by `loom session launch` in the worktree) | — |
| `LOOM_TOKEN` | bearer token the `loom` CLI / automation sends; falls back to the machine-local token file on the same host | — |
| `LOOM_OWNER_GITHUB` | GitHub login seeded as the owner on a fresh database | `rjpower` |
| `LOOM_GITHUB_CLIENT_ID` / `LOOM_GITHUB_CLIENT_SECRET` | GitHub OAuth app credentials (override the settings-stored values) | — |
| `WEAVER_TAPESTRY_DIR` | directory holding tapestry's per-session control sockets | `$WEAVER_HOME/sock` |
| `WEAVER_TAPESTRY_BIN` | the `tapestry` supervisor binary loom re-execs (else a sibling of `loom`); set by the tests | sibling of `loom` |
| `WEAVER_OVERLOOKER_AGENT_CMD` | the one-shot headless agent command behind `POST /api/agent/oneshot` (judgement calls) | `claude -p` |
| `RUST_LOG` / `EnvFilter` | tracing filter | `loom=info,weaver_core=info,tower_http=warn` |
