# AGENTS.md

Engineer-facing notes for hacking on weaver itself. For user-facing docs read
[README.md](README.md); for the prompt the in-workspace agent sees, read the
builtin [crates/weaver-core/WEAVER.md](crates/weaver-core/WEAVER.md) (a repo can
ship its own `WEAVER.md` to override it).

## Mental model

weaver ships **two binaries** and a **shared sqlite database**:

- **`weaver`** is the **agent-facing CLI**. It is database-direct (no daemon,
  no HTTP) and resolves "the current branch" from `$WEAVER_BRANCH` or, failing
  that, the git checkout under cwd. Cold start is sub-50ms; the binary
  intentionally has no `axum` / `reqwest` / SPA dependencies. Agents call
  `weaver` to read and update the goal, append notes, add and triage issues,
  and emit hook events. **`weaver` works fine whether or not `loom` is
  running** — that decoupling is the whole point of the split.

- **`loom`** is the **optional orchestrator**. It runs the REST + SSE server,
  hosts the Vue web UI, owns the per-branch tmux + agent process via the
  `sessions` table, runs the background monitor, and shells out to
  `git worktree` / `tmux`. Without loom, branches and issues still work; tmux
  orchestration, the dashboard, and the live screen do not.

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

## Layout

| Path | What's in it |
|---|---|
| `crates/weaver-core/` | lib: `branches`, `issues`, `notes`, `events`, `db`, `git`, `config`, `plan` (parser + reconcile), `repo_config` (`.weaver/config.toml`), agent helpers. Pure logic; used by both binaries. |
| `crates/weaver/src/bin/weaver.rs` | the slim agent-facing CLI (`goal`, `summary`, `readme`, `note`, `set-status` [read or set level + message], `issue …`, `where`, `log`, `hook`, `config`) |
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

## Build & test

```sh
cargo build                              # builds the backend + the Vue SPA (needs Node + npm)
cargo test --workspace                   # backend unit + integration tests (git, tmux)
cd crates/loom/frontend && npm run dev   # live-reloading SPA against `loom serve`
cd e2e && npm test                       # frontend end-to-end tests (Playwright)
```

`cargo build` builds the SPA into `static/dist` via `build.rs`; loom serves it
from there at runtime (`web::static_dir`). `rerun-if-changed` makes the SPA build
a no-op when no frontend source changed, so backend-only edits don't re-run
rspack; a Node-less checkout still builds (the backend) and serves a placeholder.
There is no skip flag — backend and frontend are separated at the **test** level
(`cargo test` for the backend, the Playwright `e2e/` suite for the frontend), not
the build level.

The integration tests shell out to real `git` and `tmux`. If one hangs, look
for stray `weaver-test-*` tmux sessions.

### Pre-commit checks

Run `./scripts/pre-commit.sh` before committing; it runs the same fmt + clippy
gate the CI `lint` job enforces. Enable it as an automatic hook with `git config
core.hooksPath .githooks`.

### Don't spin up your own `loom` — it shares the user's tmux + db

A real `loom serve` and the agent sessions it owns are **machine-global**: one
default tmux server (the `weaver-<id>` sessions) and one `~/.weaver/weaver.db`.
The user is normally running loom there with live agents inside it — including
the one running *you*. So, unless the user explicitly asks for it:

- **Don't** start your own `loom serve` / `loom launch`, create or kill
  `weaver-*` tmux sessions on the default socket, or run broad tmux cleanup
  (`tmux kill-server`, `pkill -f weaver-…`, `tmux -L 'weaver*' kill-server`).
  Each of these tears down the user's running agents at a stroke.
- **If a task seems to need a live loom session, ask the user first.**

To exercise loom or tmux behaviour, use the test infrastructure instead. Both
test suites pin tmux to a throwaway server via `WEAVER_TMUX_SOCKET`
(→ `tmux -L <name>`, see `tmux::socket_args`) and a temp `WEAVER_HOME`, so they
can never see — let alone kill — the user's real sessions: the Rust integration
tests (`crates/loom/tests/`) and the Playwright `e2e/` suite each use their own
private socket. Extend them rather than driving a real server by hand. If you
genuinely must run loom ad-hoc, isolate it the same way:

```sh
WEAVER_TMUX_SOCKET=loom-dev-$$ WEAVER_HOME=$(mktemp -d) loom serve --addr 127.0.0.1:0
```

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

## Landing changes

**Open a pull request by default.** Don't push to or merge into `main` directly.
Work on a branch, run the checks above (formatters, `cargo clippy`, and
`cargo test --workspace`), make them pass, then
`gh pr create` and let review + CI gate the merge. This holds for every change —
features, fixes, docs, refactors — unless the user explicitly tells you to
commit straight to the base branch. Agents working in a weaver worktree are
already on their own branch; finishing the work means committing it and opening
the PR, not integrating it yourself (see the builtin
[crates/weaver-core/WEAVER.md](crates/weaver-core/WEAVER.md)).

## Storage & state

- **SQLite** at `$WEAVER_HOME/weaver.db` (default `~/.weaver/weaver.db`),
  shared by `weaver` and `loom`. WAL mode handles concurrency.
  - Core tables (`weaver-core/src/db.rs`): `branches`, `issues`, `notes`,
    `events`, `settings`.
  - Loom tables (`crates/loom/src/db.rs`): `sessions`, `recent_repos`,
    `branch_github` (per-branch PR snapshot).
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
| `POST /api/sessions/{id}/{note,archive,adopt}` | actions |
| `POST /api/sessions/{id}/github` | re-poll the branch's GitHub PR now and return the updated session |
| `GET POST DELETE /api/sessions/{id}/scratch` | list / drop / remove worktree `scratch/` reference files |
| `PUT /api/sessions/{id}/file?path=…` | write raw bytes to a worktree file (the editor save primitive) |
| `GET /api/sessions/{id}/plan` | a [structured project plan](../docs/structured-projects.md), parsed + task status joined from issues |
| `POST /api/sessions/{id}/plan/sync` | reconcile a plan against the issue ledger (`apply` to write) |
| `GET /api/sessions/{id}/{diff,log,events}` | reads + SSE stream |
| `GET /api/sessions/{id}/terminal` | WebSocket: xterm.js ⇄ PTY ⇄ tmux (the interaction surface) |
| `GET /api/branches` / `GET PATCH /api/branches/{id}` | list / inspect / edit tracked branches |
| `GET POST /api/branches/{id}/issues` | issues claimed by the branch / create one |
| `GET PATCH DELETE /api/issues/{id}` | per-issue CRUD |
| `GET POST /api/repos/issues?repo_root=…` | repo-wide board (`scope=repo\|backlog`) / create a backlog item |
| `GET /api/repos/recent` / `GET /api/repos/branches?cwd=…` | recent repos / branches in a repo |
| `GET PATCH /api/settings` | settings registry |

`SessionView` (`/api/sessions[/...]`) returns session-specific fields
top-level (`id`, `status`, `work_dir`, `tmux_session`, `agent_kind`, `model`,
`effort`, `pending_prompt`, `github_repo`, `last_activity_at`,
`created_at`, `updated_at`, and — on the create response only —
`tracking_issue`) plus a nested `branch: BranchView`
(`id`, `name`, `title`, `goal`, `description`, `attention`,
`repo_root`, `branch`, `base_branch`, `created_at`, `updated_at`,
`open_issue_count`, `github`).

`BranchView::github` is the branch's latest GitHub pull-request snapshot
(`pr_number`, `pr_url`, `pr_state`, `pr_title`, `is_draft`, `review_decision`,
`checks`, `mergeable`, `merged_at`, `fetched_at`), or `null` when GitHub polling
is off, there is no PR, or `gh` is unavailable. See "GitHub integration" below.

Status is two orthogonal axes. The session's `status` is the **lifecycle**
(orchestrator-owned, mechanical): `created` / `launching` / `running` /
`orphaned` / `done` / `error`. The branch's `attention` (level) plus its
`description` (a one-line current-state message) are the **agent-declared**
"does this need me?" signal: `ok` / `attention` / `blocked`, both set via
`weaver set-status`. The dashboard filters on `attention`.

There is **no** `/api/hook` endpoint — see "Status detection" below.

**Scratch files** are reference material dropped into the worktree's `scratch/`
directory (git-ignored, so it never enters the agent's diff). They can be added
to a live session via `POST /api/sessions/{id}/scratch`, or attached up-front in
the New Session form: those ride in the create request as `scratch` and are
written *before* the agent launches, with a note appended to the launch prompt
so a fresh agent knows the files are there. The stored branch goal stays the
clean text the user typed.

## Conventions

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

**Lifecycle** is driven by Claude Code hooks. `loom launch` merges a `hooks`
block into the worktree's `.claude/settings.local.json` (see
`loom::agent::install_hooks` and `weaver_core::agent::hooks_json`):

| Claude hook event | shells out to |
|---|---|
| `SessionStart` | `weaver hook --event session-start` (also injects `additionalContext`: the repo's `WEAVER.md`, or the builtin [[crates/weaver-core/WEAVER.md]], on a genuine start/resume/clear; after a **compaction** — `source: "compact"` on the hook's stdin — a concise `weaver summary` re-orientation instead, so the agent isn't re-fed the whole guide. `weaver readme` pulls the full guide back on demand) |
| `UserPromptSubmit` | `weaver hook --event working` |
| `Notification` | `weaver hook --event waiting` |
| `Stop` | `weaver hook --event idle` |

`weaver hook` writes an `events` row keyed on the branch resolved from
`$WEAVER_BRANCH` (set by the launcher) — no HTTP. Loom's monitor (`apply_hook`)
consumes new `hook` rows on its next tick. Any hook means the agent process is
alive, so all three set `status = running` (this also promotes a freshly
`launching` session). The orchestrator no longer guesses working/waiting/idle:
liveness is all it can know for sure, so there is no stillness-based idle
demotion any more.

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

## Agent-facing commands

When working inside a worktree the agent can run, with no daemon required:

```sh
weaver goal "ship the feature"          # set the branch's goal
weaver goal                             # print the goal
weaver summary                          # goal + outstanding tasks + next-step hints
weaver readme                           # print the full weaver workflow guide (WEAVER.md)
weaver note   "blocked on the DB schema"
weaver set-status attention "ready for review"   # level + current-state message
weaver set-status ok "waiting on PR review feedback"
weaver set-status blocked                # change level only; keep the last message
weaver set-status                        # read: goal + status + open issues
weaver issue add "Backfill old rows" --body "ETA after the schema change"
weaver issue add "Audit the logger" --repo  # unclaimed repo backlog item
weaver issue ls                         # this branch's work + unclaimed backlog
weaver issue ls --mine --all            # just this branch, including closed
weaver issue ls --repo                  # whole repo, grouped by branch
weaver issue show 7                     # an issue + the live status of the branch working it
weaver issue wait 7 --timeout 600       # block until a sub-tree closes #7 or raises attention
weaver issue close 7
weaver plan new "Search rewrite"        # scaffold docs/plans/<slug>.md (large efforts)
weaver plan ls                          # plans on this branch
weaver plan show search-rewrite         # tasks + status projected from issues
weaver plan sync search-rewrite --apply # reconcile plan tasks ↔ issue ledger
weaver where                            # debug: print resolved repo / branch / branch-id
weaver log --limit 50                   # recent events for the current branch
weaver hook --event working             # (used by Claude Code hooks)
```

These are all `weaver-core` calls against the sqlite database. They write
`events` rows so that loom (if running) can react on its next monitor tick.

## Frontend notes

- Vue 3 + `<script setup>` + Vue Router. Tailwind v4 via PostCSS.
- All server calls go through `crates/loom/frontend/src/api.ts`. Don't fetch
  inline in components.
- Types in `frontend/src/types.ts` mirror the serde structs in `web.rs` —
  keep them in sync by hand (no codegen).
- The SPA is a thin client of the REST API ([[ui-built-on-rest-api]]). Don't
  invent browser-local features the `loom` CLI cannot observe.

## Environment

| Var | Purpose | Default |
|---|---|---|
| `WEAVER_HOME` | state directory | `~/.weaver` |
| `WEAVER_DB` | sqlite path | `$WEAVER_HOME/weaver.db` |
| `WEAVER_API` | loom URL (both sides — server binds, CLI talks) | `http://127.0.0.1:7878` |
| `WEAVER_BRANCH` | override the branch resolver (set by `loom launch` in the worktree) | — |
| `WEAVER_TMUX_SOCKET` | pin tmux to a dedicated server (`tmux -L <name>`) so ops can't touch real sessions; set by the test harnesses | unset → default socket |
| `RUST_LOG` / `EnvFilter` | tracing filter | `loom=info,weaver_core=info,tower_http=warn` |
