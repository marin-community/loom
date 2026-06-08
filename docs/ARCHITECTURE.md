# Architecture

Deep reference for weaver's internals. [AGENTS.md](../AGENTS.md) is the short
how-to-work guide and links here; this file is for when you need the full map.

## Mental model

weaver ships **two binaries** over a **shared sqlite database**:

- **`weaver`** — the **agent-facing CLI**: database-direct (no daemon, no HTTP),
  resolving "the current branch" from `$WEAVER_BRANCH` else the git checkout
  under cwd. Cold start is sub-50ms; it carries no `axum` / `reqwest` / SPA
  dependencies. Agents call it to read and update the goal, report status, add
  and triage issues, and emit hook events. It **works whether or not `loom` is
  running** — that decoupling is the point of the split.
- **`loom`** — the **optional orchestrator**: the REST + SSE server, the Vue web
  UI, the per-branch tmux + agent process (via the `sessions` table), the
  background monitor, and the `git worktree` / `tmux` shell-outs. Without loom,
  branches and issues still work; tmux orchestration, the dashboard, and the
  live screen do not.

```
weaver CLI ──sqlite──┐
                     ├─ ~/.weaver/weaver.db   (shared, WAL)
loom serve  ─────────┘    │
  │                       │
  ├─ axum REST + SSE      │
  ├─ tmux + git wrappers  │
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
| `crates/weaver-core/` | lib: `branches`, `issues`, `events`, `db`, `migrations` (ordered SQL + `schema_migrations` indicator), `git`, `config`, `plan` (parser + reconcile), `repo_config` (`.weaver/config.toml`), agent helpers. Pure logic; used by both binaries. |
| `crates/weaver/src/bin/weaver.rs` | the slim agent-facing CLI (`goal`, `summary`, `readme`, `set-status` [read or set level + message], `issue …`, `where`, `log`, `hook`, `config`) |
| `crates/loom/src/web.rs` | axum routes, request/response types, SSE — **the API surface** |
| `crates/loom/src/server.rs` | bind, write `server.json`, spawn bg tasks |
| `crates/loom/src/monitor.rs` | status detection, orphan marking, hook-event consumer |
| `crates/loom/src/agent.rs` | launching agents into tmux + installing `.claude/settings.local.json` hooks |
| `crates/loom/src/session.rs` | `Session` row + sqlx queries |
| `crates/loom/src/tmux.rs` | `tmux new-session / capture-pane / kill-session / attach` (exact-match `=name:` targets) |
| `crates/loom/src/terminal.rs` | WebSocket ⇄ PTY bridge: xterm.js ⇄ `tmux attach` (the live terminal) |
| `crates/loom/src/github.rs` | `gh` CLI shell-out: issue seeding, PR opening, and the PR-status poll loop (snapshots each branch's PR; archives on merge) |
| `crates/loom/src/client.rs` | HTTP client used by the `loom` CLI to talk to its own daemon |
| `crates/loom/src/bin/loom.rs` | the orchestrator CLI (`serve`, `launch`, `ps`, `attach`, …) |
| `crates/loom/frontend/` | Vue 3 SPA, rspack, Tailwind. `api.ts` + views in `views/` |
| `crates/loom/static/dist/` | Build output (placeholder; real build overwrites) |
| `crates/loom/tests/` | integration tests: `integration/` (server suites) + `hook_monitor.rs`; need `git` + `tmux` |
| `e2e/` | Playwright; talks to a real `loom serve`. Separate `package.json` |
| `crates/loom/build.rs` | Builds the SPA into `static/dist` (npm + rspack); writes a placeholder when Node is unavailable |

## Build internals

`cargo build` builds the SPA into `static/dist` via `build.rs`; loom serves it
from there at runtime (`web::static_dir`). `rerun-if-changed` makes the SPA build
a no-op when no frontend source changed, so backend-only edits don't re-run
rspack; a Node-less checkout still builds (the backend) and serves a placeholder.
There is no skip flag — backend and frontend are separated at the **test** level
(`cargo test` for the backend, the Playwright `e2e/` suite for the frontend), not
the build level.

The integration tests shell out to real `git` and `tmux`. If one hangs, look
for stray `weaver-test-*` tmux sessions.

### Agent lint review

After the fmt + clippy gate, `scripts/pre-commit.sh` runs
`scripts/lint-review.py` — a self-contained `uv run` script (the call is gated on
`uv` being on PATH, so the CI lint job, which has neither `uv` nor an agent, runs
only fmt + clippy). It catches the *agent slop* fmt and clippy can't — the
judgement calls: naming, API shape, dead/speculative code, duplication, and
comment/test quality. It builds one prompt from the [`docs/lint.md`](lint.md)
catalog (`wl-...` rules) plus the diff and pipes it to a headless `claude -p`
sub-agent, run as a fresh session — the calling session's `CLAUDE_CODE_*` /
`ANTHROPIC_API_KEY` markers are stripped so it neither nests in the caller's
transcript nor bills the metered API. It parses the findings and **errors on any
at or above a confidence threshold**; the rest print as advisory.

`pre-commit.sh` reviews the **staged** diff (`--staged`); run `lint-review.py`
bare to review the whole branch against its merge-base with `main`. It
**self-skips** (exit 0, never blocks a commit) when `claude` isn't on PATH, when
there are no Rust/TS/Vue changes, or when the agent times out or errors — so CI,
which has no agent, runs only the fmt+clippy gate, and a flaky agent can't wedge
every commit. Only real findings block.

Knobs: `WEAVER_SKIP_AGENT_LINT=1` to skip a run, `WEAVER_LINT_MIN_CONFIDENCE`
(default `0.9`), `WEAVER_LINT_AGENT_CMD` (default `claude -p`), and
`WEAVER_LINT_TIMEOUT` (default `600`s). Suppress a false positive with a trailing
`// wl-allow: <code>` on the cited line; bypass the whole hook once with `git
commit --no-verify`.

### End-to-end (Playwright)

The `e2e/` suite drives the real UI against a real server. It boots **one**
`loom serve` per Playwright *worker* (not per test) on a random port, each with
its own `WEAVER_HOME` / sqlite db, a private tmux socket (`WEAVER_TMUX_SOCKET`,
reaped on teardown), and a throwaway git repo (see `e2e/fixtures/weaver.ts`),
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
    `branch_github` (per-branch PR snapshot).
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
  committed `.weaver/config.toml` read by `weaver-core::repo_config` (today just
  `[plan].dir`, default `docs/plans`) — distinct from the settings table, and
  resolved repo-file → builtin-default like a repo's own `WEAVER.md`.
- **Worktrees** live under `<repo>/.worktrees/<slug>` on `weaver/<slug>`
  (unless `--branch` reused an existing branch).

## REST API

All routes live under `/api`. The Vue SPA is the primary consumer.

| Method + path | What it does |
|---|---|
| `GET /api/health` | liveness probe |
| `GET /api/sessions` / `POST /api/sessions` | list / create sessions (create takes optional `scratch: [{name, content_base64}]` and `parent_branch`; opens a tracking issue and returns its id as `tracking_issue`) |
| `GET PATCH DELETE /api/sessions/{id}` | session CRUD (status, title, goal, description, attention) |
| `POST /api/sessions/{id}/{archive,adopt}` | actions |
| `POST /api/sessions/{id}/github` | re-poll the branch's GitHub PR now and return the updated session |
| `GET POST DELETE /api/sessions/{id}/scratch` | list / drop / remove worktree `scratch/` reference files |
| `PUT /api/sessions/{id}/file?path=…` | write raw bytes to a worktree file (the editor save primitive) |
| `GET /api/sessions/{id}/plan` | a [structured project plan](structured-projects.md), parsed + task status joined from issues |
| `POST /api/sessions/{id}/plan/sync` | reconcile a plan against the issue ledger (`apply` to write) |
| `GET /api/sessions/{id}/{diff,log,events}` | reads + SSE stream |
| `GET /api/sessions/{id}/terminal` | WebSocket: xterm.js ⇄ PTY ⇄ tmux (the interaction surface) |
| `POST /api/sessions/{id}/send` | type `{text}` into the agent's tmux pane; `submit` (default true) follows it with Enter to trigger a round |
| `POST /api/sessions/{id}/interrupt` | send a break (Escape) to the pane — stop the current turn |
| `GET /api/sessions/{id}/preview?lines=N` | capture the pane as `{screen}`; `lines` adds scrollback above the visible screen |
| `GET /api/branches` / `GET PATCH /api/branches/{id}` | list / inspect / edit tracked branches |
| `GET POST /api/branches/{id}/issues` | issues claimed by the branch / create one |
| `GET PATCH DELETE /api/issues/{id}` | per-issue CRUD |
| `GET POST /api/repos/issues?repo_root=…` | repo-wide board (`scope=repo\|backlog`) / create a backlog item |
| `GET /api/repos/recent` / `GET /api/repos/branches?cwd=…` | recent repos / branches in a repo |
| `GET PATCH /api/settings` | settings registry |

`SessionView` (`/api/sessions[/...]`) returns session-specific fields
top-level (`id`, `status`, `work_dir`, `tmux_session`, `agent_kind`, `model`,
`effort`, `pending_prompt`, `github_repo`, `last_activity_at`,
`created_at`, `updated_at`, `parent_id`, and — on the create response only —
`tracking_issue`) plus a nested `branch: BranchView`
(`id`, `name`, `title`, `goal`, `description`, `attention`,
`repo_root`, `branch`, `base_branch`, `created_at`, `updated_at`,
`open_issue_count`, `github`).

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
`orphaned` / `done` / `error`. The branch's `attention` (level) plus its
`description` (a one-line current-state message) are the **agent-declared**
"does this need me?" signal: `ok` / `attention` / `blocked`, both set via
`weaver set-status`. The dashboard filters on `attention`.

There is **no** `/api/hook` endpoint — see [Status & attention](#status--attention).

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

**Driving the pane.** `send` / `interrupt` / `preview` are one-shot HTTP
primitives over `tmux send-keys` and `capture-pane` (see `tmux::send_literal`,
`send_key`, `capture`), distinct from the interactive terminal WebSocket: they
let an agent or script type into, interrupt, or read back a child session
uniformly. Each requires a live tmux (else 409). The CLI's `loom session
{send,break,preview}` wrap them.

## Runtime conventions

- **API-first.** New features land as a REST endpoint in `web.rs` first; the
  SPA and the `loom` CLI both consume it. Don't put business logic in
  `bin/loom.rs` or in the Vue layer.
- **Errors:** the server returns `AppError` (status + message + optional
  `details` map of per-field reasons); the `loom` CLI uses `anyhow` and prints
  `error: {e:#}`.
- **Async:** tokio everywhere on the server side. Long-running subprocesses
  (tmux, git, gh, the agent) go through `tokio::process::Command`. The
  `weaver` CLI is synchronous-feeling (just a few `sqlx` calls per command).
- **Events:** state changes flow through `EventBus`; the SSE handler in
  `web.rs` fans them out. `weaver hook` writes directly to the `events`
  table, and loom's monitor tick promotes the new row into a session status
  change and a fresh `EventBus` notification.
- **No tracking-branch state in the server:** loom can be killed and restarted
  at any time. tmux sessions and worktrees survive; "orphaned" is a
  first-class status, recovered via `loom adopt` (or the Adopt button in the
  UI).

## Status & attention

Two distinct axes (see the SessionView note above): the mechanical **lifecycle**
`sessions.status`, and the agent-declared **attention** on the branch.

**Lifecycle** is driven by Claude Code hooks. `loom session launch` merges a `hooks`
block into the worktree's `.claude/settings.local.json` (see
`loom::agent::install_hooks` and `weaver_core::agent::hooks_json`):

| Claude hook event | shells out to |
|---|---|
| `SessionStart` | `weaver hook --event session-start` (also injects `additionalContext`: the repo's `WEAVER.md`, or the builtin [crates/weaver-core/WEAVER.md](../crates/weaver-core/WEAVER.md), on a genuine start/resume/clear; after a **compaction** — `source: "compact"` on the hook's stdin — a concise `weaver summary` re-orientation instead, so the agent isn't re-fed the whole guide. `weaver readme` pulls the full guide back on demand) |
| `UserPromptSubmit` | `weaver hook --event working` |
| `Notification` | `weaver hook --event waiting` |
| `Stop` | `weaver hook --event idle` |

`weaver hook` writes an `events` row keyed on the branch resolved from
`$WEAVER_BRANCH` (set by the launcher) — no HTTP. Loom's monitor (`apply_hook`)
consumes new `hook` rows on its next tick. Any hook means the agent process is
alive, so all three set `status = running` (this also promotes a freshly
`launching` session). Liveness is all a hook proves, so that is all the
orchestrator tracks — it does not infer working/waiting/idle from stillness.

The hooks also nudge the **attention** level where they carry a genuine signal:
`working` clears it to `ok` and drops any pending prompt (the user is engaged);
`waiting` raises it to `attention` and snapshots the tmux pane into
`pending_prompt` (Claude is blocked asking the user — the snapshot conveys what
it's waiting on, so no separate note is stored); `idle` (a turn ending) leaves
attention untouched, so a finished-but-fine agent isn't mistaken for one that
needs you.

**Attention** is otherwise the agent's own call, set via `weaver set-status
<level> ["<message>"]`. That writes the branch's `attention` level (and, when a
message is given, the `description`) directly (daemon-less) and an `attention`
event the monitor re-broadcasts over SSE. A bare `weaver set-status <level>`
changes only the level and keeps the last message. Last write wins, so an
explicit declaration overrides the hook-inferred default. The PATCH
`/api/sessions/{id}` and `/api/branches/{id}` routes accept `attention` (and
`description`) too, for the UI.

Archiving a session clears its attention back to `ok` (and drops any snapshotted
`pending_prompt`): the agent is gone, so a torn-down workstream can't still "need
me", and the dashboard stops flagging it. The UI also treats any `archived`
session as `ok` regardless of a stale attention value left on the branch.

Orphan detection is independent: if `tmux has-session` says no, the session
becomes `orphaned` and is eligible for `loom adopt`.

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
code): tmux killed, worktree removed, branch and weaver history kept. The
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

## Environment

| Var | Purpose | Default |
|---|---|---|
| `WEAVER_HOME` | state directory | `~/.weaver` |
| `WEAVER_DB` | sqlite path | `$WEAVER_HOME/weaver.db` |
| `WEAVER_API` | loom URL (both sides — server binds, CLI talks) | `http://127.0.0.1:7878` |
| `WEAVER_BRANCH` | override the branch resolver (set by `loom session launch` in the worktree) | — |
| `WEAVER_TMUX_SOCKET` | pin tmux to a dedicated server (`tmux -L <name>`) so ops can't touch real sessions; set by the test harnesses | unset → default socket |
| `RUST_LOG` / `EnvFilter` | tracing filter | `loom=info,weaver_core=info,tower_http=warn` |
