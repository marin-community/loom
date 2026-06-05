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
loom summary <branch>                 # force a fresh summary now
loom archive <branch>                 # tear down tmux + worktree, keep branch + history
loom adopt <branch>                   # recreate tmux for an orphaned session
loom rm <branch>                      # remove worktree + tmux + db row
loom open                             # open the web UI

# Agent-facing (run from inside the worktree, no daemon required)
weaver goal "ship the feature"
weaver goal                           # print the current goal
weaver describe "Wired up routes; tests pass."
weaver note    "blocked on the DB schema"
weaver status attention "ready for review"   # how the agent is doing (ok|attention|blocked)
weaver issue add "Backfill old rows" --body "ETA after the schema change"
weaver issue add "Audit the logger" --repo   # unclaimed repo backlog item
weaver issue ls                       # this branch's work + the repo backlog
weaver issue ls --mine                # just this branch's claimed issues
weaver issue ls --repo                # the whole repo, grouped by branch
weaver issue close 7
weaver status                         # goal + attention + open-issue count
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
hook), the monitor snapshots the tmux pane into `pending_prompt` so the UI can
show what the agent is waiting on.

The **attention** axis is the agent's own signal of whether it needs you:
`ok` (going fine, or blocked on something external like a CI run or PR review),
`attention` (a question, a decision, "ready for review"), or `blocked` (stuck,
needs help). Agents set it with `weaver status <level> "<note>"`; the dashboard
shows it and lets you filter for sessions that need a human. It replaces the old
guessed working/waiting/idle indicator, which was often wrong — e.g. it read
"idle" while the agent was actually waiting on a background workflow.

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

## REST API (brief)

Loom serves a JSON API under `/api`; the Vue SPA is the primary consumer.

- `GET /api/health`
- `GET POST /api/sessions`, `GET PATCH DELETE /api/sessions/{id}`,
  `POST /api/sessions/{id}/{note,summarize,archive,adopt}`,
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
weaver config get agent.summary_command
weaver config set agent.claude_args "--model claude-opus-4-7"
weaver config unset agent.claude_args
```

Notable settings:

- `agent.default` — agent kind launched for a new session when `loom launch`
  is given no `--agent` (`claude`, `shell`, or a custom command).
- `agent.claude_args` — extra arguments spliced into the Claude TUI launch,
  e.g. `--model claude-opus-4-7`.
- `agent.summary_command` — command used for the headless diff summaries.
- `summary.interval_secs` — how often the background summarizer revisits an
  active session.
- `server.auto_adopt` — adopt every recoverable session on daemon startup.

## Building

```sh
cargo build                              # builds the Vue frontend too (needs Node + npm)
WEAVER_SKIP_FRONTEND=1 cargo build       # backend only
cargo test --workspace                   # unit + integration (needs git, tmux)
```

## Environment

- `WEAVER_HOME` — state directory (default `~/.weaver`)
- `WEAVER_DB` — database path (default `$WEAVER_HOME/weaver.db`)
- `WEAVER_API` — loom URL the `loom` CLI talks to (default `http://127.0.0.1:7878`)
- `WEAVER_BRANCH` — override the branch resolver (set by `loom launch` in the worktree)
- `WEAVER_SKIP_FRONTEND` — skip `npm run build` in `build.rs`
