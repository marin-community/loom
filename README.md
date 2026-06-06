# weaver

A lightweight per-branch task tracker, plus an optional orchestrator that runs
coding agents in tmux.

weaver ships two binaries:

- **`weaver`** — the **agent-facing CLI**. It is database-direct (no daemon,
  no HTTP) and runs sub-50ms cold. The agent inside a worktree uses it to
  read and update the branch's **goal**, **description**, **notes**, and the
  repo's **issues** (each claimed by a branch or sitting in the shared
  backlog). It works whether or not the orchestrator is running.
- **`loom`** — the **optional orchestrator**. It runs the REST + SSE server,
  hosts a Vue dashboard, creates worktrees, launches agents into tmux, and
  periodically summarizes each branch's diff against its merge base. Without
  loom, branches and issues still work; tmux + the dashboard do not.

Both binaries share one sqlite database at `~/.weaver/weaver.db`.

## Architecture

```
weaver CLI ──sqlite──┐
                     ├─ ~/.weaver/weaver.db   (shared, WAL mode)
loom serve  ─────────┘
  │
  ├─ axum REST + SSE (127.0.0.1:7878)
  ├─ tmux + git worktree wrappers
  ├─ agent launcher (Claude / shell / custom command)
  ├─ background monitor (status, orphan detection, hook ingest)
  ├─ background summarizer (headless agent → branch description)
  └─ Vue SPA at /
```

Agents call `weaver hook` to report status; the loom monitor sees the new
`events` row on its next tick and flips the session's status. This is the
key decoupling — the agent CLI never speaks HTTP.

## Usage

```sh
# Orchestrator (optional)
loom serve                            # run the daemon (REST + UI + tmux + monitor)
loom launch "Add a /health endpoint"  # new worktree + tmux + agent, seeded with the task
loom launch "Refactor the parser" --name parser-refactor   # override the branch slug
loom launch "Big refactor" --model opus --effort high      # pick model tier + reasoning effort
loom ps                               # list active sessions
loom show <branch>                    # session detail
loom attach <branch>                  # exec tmux attach (or use the browser terminal)
loom archive <branch>                 # tear down tmux + worktree, keep branch + history
loom adopt <branch>                   # recreate tmux for an orphaned session
loom rm <branch>                      # remove worktree + tmux + db row
loom open                             # open the web UI

# Agent-facing (run from inside the worktree, no daemon required)
weaver goal "ship the feature"
weaver goal                           # print the current goal
weaver summary                        # goal + outstanding tasks + next-step hints
weaver set-status attention "ready for review"   # level (ok|attention|blocked) + current-state message
weaver issue add "Backfill old rows" --body "ETA after the schema change"
weaver issue add "Audit the logger" --repo   # unclaimed repo backlog item
weaver issue ls                       # this branch's work + the repo backlog
weaver issue ls --mine                # just this branch's claimed issues
weaver issue ls --repo                # the whole repo, grouped by branch
weaver issue close 7
weaver issue show 7                   # an issue + the live status of the branch working it
weaver issue wait 7                   # block until a sub-session finishes or needs you
weaver set-status                     # read: goal + status + open-issue count
weaver where                          # debug: print resolved repo / branch / branch-id
weaver log --limit 50                 # recent events for the current branch
```

`loom launch`'s positional argument is the **task**: it becomes the branch goal
and the agent's opening prompt, and the `weaver/<slug>` branch name is derived
from it (override with `--name`). The agent is `claude` unless you pass
`--agent` or change `agent.default`, so the common case is just `loom launch
"<what to do>"`. A `loom launch` with no task and nothing to pick up prints a
usage hint and exits without launching.

Three flags seed the task from existing work instead of a fresh description:
`loom launch --issue 123` takes the branch's title / goal / description from a
GitHub issue (via the `gh` CLI), `loom launch --claim 7` takes them from an
existing weaver issue and moves it out of the repo backlog, and `loom launch
--branch <name>` resumes an existing branch. `loom issues` prints the repo's
board across branches.

Every launch opens a **tracking issue** claimed by the new branch — the task as
a weaver issue — and `loom launch` prints its number. That number is the handle
for following the session: `weaver issue show <n>` reports the issue plus the
live `set-status` of the branch working it, and `weaver issue wait <n>` blocks
until the issue closes or that branch raises its attention. The launched agent
is told to keep its status current and close the issue when the work is done.
When an agent already inside a weaver session runs `loom launch`, the tracking
issue is attributed to it (`source_branch`), so its sub-trees show up under
"Delegated by this branch" in `weaver issue ls` — agents can fan work out into
parallel sub-sessions and poll or block on them the same way a human does.

`loom launch --model <haiku|sonnet|opus> --effort <low|medium|high|xhigh|max>`
(both also selectors in the web create form) pin the session's Claude model
tier and reasoning effort. They are orthogonal — any model runs at any effort —
and spliced into the launch as `--model` / `--effort`. Both are stored per
session, so adopting a recovered session resumes with the same settings. Omit
either to inherit `agent.claude_args`.

## Status & attention

Status has two independent axes.

The **lifecycle** (`session.status`) is mechanical and orchestrator-owned:
`created`, `launching`, `running`, `orphaned`, `done`, or `error`. `done` and
`error` are terminal; the rest, including `orphaned`, are recoverable. Claude-
backed sessions drive it via Claude Code hooks installed into
`.claude/settings.local.json` by `loom launch`. Each hook shells out to
`weaver hook --event {working|waiting|idle|session-start}`, writing an `events`
row the monitor consumes on its next tick; any hook means the agent process is
alive → `running`. When Claude blocks asking the user (the `waiting`/Notification
hook), the monitor raises the branch's **attention** to `attention`; the live
prompt itself is read straight from the terminal, one tab away.

The **attention** axis is the agent's own signal of whether it needs you:
`ok` (going fine, or blocked on something external like a CI run or PR review),
`attention` (a question, a decision, "ready for review"), or `blocked` (stuck,
needs help). Agents set it with `weaver set-status <level> "<message>"`, which
records both the level and a one-line current-state message; a bare
`weaver set-status <level>` changes the level and keeps the last message. The
dashboard shows both and lets you filter for sessions that need a human. It
replaces the old guessed working/waiting/idle indicator, which was often wrong —
e.g. it read "idle" while the agent was actually waiting on a background
workflow.

## Adoption

A session's tmux process is independent of the loom daemon: it does not
survive a machine reboot, though the sqlite rows and worktrees do. When the
monitor finds a session whose tmux has vanished, it marks it `orphaned`
rather than `done`.

An orphaned session can be adopted — its tmux session recreated and its
agent resumed (`claude --continue`):

```sh
loom adopt <branch>                   # or the "Adopt" button in the web UI
```

Set `server.auto_adopt` to have loom adopt every recoverable session
automatically on startup (off by default):

```sh
weaver config set server.auto_adopt true
```

## GitHub

With the `gh` CLI installed and authenticated, loom tracks each active session's
pull request. A background loop polls `gh pr view` for the branch every 30s and
surfaces the result on the dashboard: a link straight to the PR, its state
(open / draft / merged / closed), the review decision (approved / changes
requested / review required), and a rolled-up CI verdict (checks passing /
failing / pending). The session's Overview tab has a **Refresh** button to
re-poll on demand.

Once a branch's PR merges, loom archives the session automatically — tearing
down its tmux and worktree while keeping the branch and its weaver history, the
same as the Archive button. Turn either behaviour off in **Settings** or from
the CLI:

```sh
weaver config set github.archive_on_merge false   # keep the worktree after merge
weaver config set github.poll false               # stop polling GitHub entirely
```

Polling is a quiet no-op for repositories without a GitHub remote, or wherever
`gh` is not installed — nothing to configure to opt out there.

## REST API (brief)

Loom serves a JSON API under `/api`; the Vue SPA is the primary consumer.

- `GET /api/health`
- `GET POST /api/sessions`, `GET PATCH DELETE /api/sessions/{id}`,
  `POST /api/sessions/{id}/{note,archive,adopt,github}`,
  `GET /api/sessions/{id}/{diff,log,events}`,
  `GET /api/sessions/{id}/terminal` (WebSocket: xterm.js ⇄ PTY ⇄ tmux)
- `GET /api/branches`, `GET PATCH /api/branches/{id}`,
  `GET POST /api/branches/{id}/issues` (issues claimed by the branch),
  `GET PATCH DELETE /api/issues/{id}`
- `GET /api/repos/recent`, `GET /api/repos/branches?cwd=…`,
  `GET POST /api/repos/issues?repo_root=…` (the repo-wide board / backlog)
- `GET PATCH /api/settings`

See `AGENTS.md` for the shape of `SessionView`.

## Server address

`loom serve` binds `127.0.0.1:7878` by default. Set `WEAVER_API` (e.g.
`WEAVER_API=http://127.0.0.1:9000`) to point loom *and* the `loom` CLI at a
different address. `loom serve --addr <host:port>` overrides `WEAVER_API`.
The running daemon records the address it bound in `~/.weaver/server.json`,
so the `loom` CLI finds it with no configuration in the common case.

## Configuration

Settings live in the `settings` table of the sqlite database, shared by both
binaries. Each known setting is declared in a registry (`config.rs`) with a
label, help text, type, and default.

Edit them in the **Settings** pane of the web UI, or from the CLI:

```sh
weaver config list
weaver config get agent.default
weaver config set agent.claude_args "--model claude-opus-4-7"
weaver config unset agent.claude_args
```

Notable settings:

- `agent.default` — agent kind launched for a new session when `loom launch`
  is given no `--agent` (`claude`, `shell`, or a custom command).
- `agent.claude_args` — extra arguments spliced into the Claude TUI launch,
  e.g. `--model claude-opus-4-7`.
- `server.auto_adopt` — adopt every recoverable session on daemon startup.
- `github.poll` — poll GitHub (via `gh`) for each session's PR, review, and
  check status (on by default; a no-op without `gh` or a GitHub remote).
- `github.archive_on_merge` — archive a session automatically once its PR
  merges (on by default).

## Building

```sh
cargo build                              # builds the backend + the Vue SPA (needs Node + npm)
cargo test --workspace                   # backend unit + integration tests (need git, tmux)
cd e2e && npm test                       # frontend end-to-end tests (Playwright)
```

`cargo build` builds the SPA into `crates/loom/static/dist` (via `build.rs`),
which `loom serve` serves at runtime; `rerun-if-changed` keeps it a no-op when no
frontend source changed, so backend-only edits don't re-run rspack. On a checkout
without Node the build still succeeds and serves a placeholder page. Backend
tests are the Rust suites; the frontend's tests are the Playwright `e2e/` suite.

## Environment

- `WEAVER_HOME` — state directory (default `~/.weaver`)
- `WEAVER_DB` — database path (default `$WEAVER_HOME/weaver.db`)
- `WEAVER_API` — loom URL the `loom` CLI talks to (default `http://127.0.0.1:7878`)
- `WEAVER_BRANCH` — override the branch resolver (set by `loom launch` in the worktree)
