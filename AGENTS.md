# AGENTS.md

Engineer-facing notes for hacking on weaver itself. For user-facing docs read
[README.md](README.md); for the prompt the in-workspace agent sees, read
[crates/weaver-core/primer.md](crates/weaver-core/primer.md).

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
  `sessions` table, runs the background monitor and periodic summarizer, and
  shells out to `git worktree` / `tmux`. Without loom, branches and issues
  still work; tmux orchestration, the dashboard, and the live screen do not.

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
| `crates/weaver-core/` | lib: `branches`, `issues`, `notes`, `events`, `db`, `git`, `config`, agent helpers. Pure logic; used by both binaries. |
| `crates/weaver/src/bin/weaver.rs` | the slim agent-facing CLI (`goal`, `describe`, `note`, `status` [read or set attention], `issue …`, `where`, `log`, `hook`, `config`) |
| `crates/loom/src/web.rs` | axum routes, request/response types, SSE — **the API surface** |
| `crates/loom/src/server.rs` | bind, write `server.json`, spawn bg tasks |
| `crates/loom/src/monitor.rs` | status detection, orphan marking, hook-event consumer |
| `crates/loom/src/summary.rs` | background summarizer |
| `crates/loom/src/agent.rs` | launching agents into tmux + installing `.claude/settings.local.json` hooks |
| `crates/loom/src/session.rs` | `Session` row + sqlx queries |
| `crates/loom/src/tmux.rs` | `tmux new-session / capture-pane / kill-session / attach` (exact-match `=name:` targets) |
| `crates/loom/src/terminal.rs` | WebSocket ⇄ PTY bridge: xterm.js ⇄ `tmux attach` (the live terminal) |
| `crates/loom/src/github.rs` | `gh` CLI shell-out for issue seeding |
| `crates/loom/src/client.rs` | HTTP client used by the `loom` CLI to talk to its own daemon |
| `crates/loom/src/bin/loom.rs` | the orchestrator CLI (`serve`, `launch`, `ps`, `attach`, …) |
| `crates/loom/frontend/` | Vue 3 SPA, rspack, Tailwind. `api.ts` + views in `views/` |
| `crates/loom/static/dist/` | Build output (placeholder; real build overwrites) |
| `crates/loom/tests/` | integration tests (need `git` + `tmux`) |
| `e2e/` | Playwright; talks to a real `loom serve`. Separate `package.json` |
| `crates/loom/build.rs` | Runs `npm run build` in `frontend/`. Honors `WEAVER_SKIP_FRONTEND` |

## Build & test

```sh
cargo build                           # also runs `npm run build` in the SPA
WEAVER_SKIP_FRONTEND=1 cargo build    # backend only — fastest iteration
WEAVER_SKIP_FRONTEND=1 cargo test --workspace
cd crates/loom/frontend && npm run dev  # live-reloading SPA against `loom serve`
cd e2e && npm test                    # Playwright suite
```

The integration test shells out to real `git` and `tmux`. If it hangs, look
for stray `weaver-test-*` tmux sessions.

## Storage & state

- **SQLite** at `$WEAVER_HOME/weaver.db` (default `~/.weaver/weaver.db`),
  shared by `weaver` and `loom`. WAL mode handles concurrency.
  - Core tables (`weaver-core/src/db.rs`): `branches`, `issues`, `notes`,
    `events`, `settings`.
  - Loom tables (`crates/loom/src/db.rs`): `sessions`, `summaries`,
    `recent_repos`.
- **`server.json`** in `$WEAVER_HOME`: pid + bound addr, written when `loom`
  comes up. The `loom` CLI uses it to find the daemon when `WEAVER_API` is
  unset.
- **Settings** live in the `settings` table; each key is declared in
  `weaver-core::config::registry()`. Both binaries read it.
- **Worktrees** live under `<repo>/.worktrees/<slug>` on `weaver/<slug>`
  (unless `--branch` reused an existing branch).

## REST API

All routes live under `/api`. The Vue SPA is the primary consumer.

| Method + path | What it does |
|---|---|
| `GET /api/health` | liveness probe |
| `GET /api/sessions` / `POST /api/sessions` | list / create sessions |
| `GET PATCH DELETE /api/sessions/{id}` | session CRUD (status, title, goal, description) |
| `POST /api/sessions/{id}/{note,summarize,merge,adopt}` | actions |
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
`effort`, `pending_prompt`, `github_repo`, `last_activity_at`, `summary_updated_at`,
`created_at`, `updated_at`) plus a nested `branch: BranchView`
(`id`, `name`, `title`, `goal`, `description`, `attention`, `attention_note`,
`repo_root`, `branch`, `base_branch`, `created_at`, `updated_at`,
`open_issue_count`).

Status is two orthogonal axes. The session's `status` is the **lifecycle**
(orchestrator-owned, mechanical): `created` / `launching` / `running` /
`orphaned` / `done` / `error`. The branch's `attention` (+ `attention_note`) is
the **agent-declared** "does this need me?" signal: `ok` / `attention` /
`blocked`, set via `weaver status`. The dashboard filters on `attention`.

There is **no** `/api/hook` endpoint — see "Status detection" below.

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
| `SessionStart` | `weaver hook --event session-start` (also injects [[crates/weaver-core/primer.md]] as `additionalContext`) |
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

The hooks also nudge **attention** where they carry a genuine signal:
`working` clears it to `ok` and drops any pending prompt (the user is engaged);
`waiting` raises it to `attention` ("Waiting for input") and snapshots the tmux
pane into `pending_prompt` (Claude is blocked asking the user); `idle` (a turn
ending) leaves attention untouched, so a finished-but-fine agent isn't mistaken
for one that needs you.

**Attention** is otherwise the agent's own call, set via `weaver status <level>
["<note>"]`. That writes the branch's `attention`/`attention_note` columns
directly (daemon-less, like `weaver describe`) and an `attention` event the
monitor re-broadcasts over SSE. Last write wins, so an explicit declaration
overrides the hook-inferred default. The PATCH `/api/sessions/{id}` and
`/api/branches/{id}` routes accept `attention`/`attention_note` too, for the UI.

Orphan detection is independent: if `tmux has-session` says no, the session
becomes `orphaned` and is eligible for `loom adopt`.

## Agent-facing commands

When working inside a worktree the agent can run, with no daemon required:

```sh
weaver goal "ship the feature"          # set the branch's goal
weaver goal                             # print the goal
weaver describe "Wired up routes; tests pass"
weaver note   "blocked on the DB schema"
weaver status attention "ready for review"   # declare how the agent is doing
weaver status ok "waiting on PR review feedback"
weaver status                           # read goal + attention + open issues
weaver issue add "Backfill old rows" --body "ETA after the schema change"
weaver issue add "Audit the logger" --repo  # unclaimed repo backlog item
weaver issue ls                         # this branch's work + unclaimed backlog
weaver issue ls --mine --all            # just this branch, including closed
weaver issue ls --repo                  # whole repo, grouped by branch
weaver issue close 7
weaver where                            # debug: print resolved repo / branch / branch-id
weaver log --limit 50                   # recent events for the current branch
weaver status                           # title + goal + open-issue count
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
| `WEAVER_SKIP_FRONTEND` | skip `npm run build` in `build.rs` | unset |
| `RUST_LOG` / `EnvFilter` | tracing filter | `loom=info,weaver_core=info,tower_http=warn` |
