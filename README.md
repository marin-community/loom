# weaver

A manager + launcher for concurrent agent workstreams.

The unit of work is a **workspace**: one git worktree + one tmux session running
a coding agent, with a tracked high-level **goal** and an evolving
**description** of its current state. weaver creates the worktree, launches the
agent into tmux, lets you observe and nudge any session, and periodically runs a
headless agent to summarize each worktree's diff against its merge base.

There is no task DAG and no orchestration — each workspace is independent, and
the agent inside it manages its own work.

## Architecture

```
weaver CLI ──HTTP──▶ weaver server (axum, 127.0.0.1:7878)
                       ├─ SQLite DB (~/.weaver/weaver.db)   one DB per machine
                       ├─ tmux   (new / send / capture / kill)
                       ├─ git    (worktree add/remove, diff, merge-base, merge)
                       ├─ agent launch (claude in tmux) + headless summaries
                       └─ background tasks: screen monitor, summarizer
Claude Code hooks ──HTTP──▶ server   (working / waiting / idle status)
Vue SPA ──REST + SSE──▶ server
```

The CLI is a thin HTTP client; `weaver serve` owns the database, tmux, and git.
`weaver attach` is the only command that runs locally — it `exec`s `tmux attach`.

## Usage

```sh
weaver serve                          # run the server (also serves the web UI)
weaver new "add a /health endpoint"   # create a workspace in the current repo
weaver ls                             # list workspaces
weaver status <id>                    # workspace detail
weaver attach <id>                    # attach to the agent's tmux session
weaver send <id> "use port 8081"      # send a line to an idle agent
weaver interrupt <id>                 # interrupt the agent (sends Esc)
weaver summary <id>                   # force a state summary now
weaver merge <id>                     # merge the branch into its base
weaver adopt <id>                     # recreate the tmux session for an orphaned workspace
weaver rm <id>                        # remove the worktree + tmux session
weaver open                           # open the web UI
```

Run inside a worktree, agents report progress with:

```sh
weaver goal                           # print the workspace goal
weaver description "wired up routes"  # set the current-state description
weaver note "blocked on the DB schema"
```

The `weaver` binary is put on the agent's `PATH` automatically, and a
SessionStart hook primes each session with these commands and the expectation
to record decisions and keep going rather than block on the user. The primer
text lives in [`primer.md`](primer.md).

`weaver new --issue 123` seeds the goal/description from a GitHub issue (via the
`gh` CLI).

## Status detection

A workspace's status is one of `created`, `launching`, `working`, `waiting`,
`idle`, `orphaned`, `done`, or `error`. `done` and `error` are terminal;
the rest, including `orphaned`, are recoverable.

claude-backed workspaces report status via Claude Code hooks installed into
`.claude/settings.local.json` (`working` / `waiting` / `idle`). Other agents
fall back to tmux screen-stillness detection. When a workspace goes `waiting`,
weaver snapshots the agent's tmux pane into `pending_prompt` so the dashboard
(and `weaver status <id>`) can show what it is blocked on.

## Adoption

A workspace's tmux session is independent of the weaver server: it does not
survive a machine reboot, though the SQLite rows and git worktrees do. When the
monitor finds a workspace whose tmux session has vanished, it marks it
`orphaned` rather than `done`.

An orphaned workspace can be **adopted** — its tmux session recreated and its
agent resumed (`claude --continue`, which continues the most recent
conversation rather than restarting from the goal):

```sh
weaver adopt <id>                     # or the "Adopt" button in the web UI
```

Set `server.auto_adopt` to have the server adopt every recoverable workspace
automatically on startup (off by default):

```sh
weaver config set server.auto_adopt true
```

## Server address

`weaver serve` binds `127.0.0.1:7878` by default. Set `WEAVER_API` (e.g.
`WEAVER_API=http://127.0.0.1:9000`) to point the server *and* every CLI client
at a different address — it configures both sides. The running server records
the address it actually bound in `~/.weaver/server.json`, so clients find it
with no configuration in the common case. An explicit `weaver serve --addr
<host:port>` overrides `WEAVER_API`.

## Building

```sh
cargo build                 # builds the Vue frontend too (needs Node + npm)
WEAVER_SKIP_FRONTEND=1 cargo build   # backend only
cargo test                  # unit tests + an integration test (needs git, tmux)
```

## Environment

- `WEAVER_HOME` — state directory (default `~/.weaver`)
- `WEAVER_DB` — database path (default `$WEAVER_HOME/weaver.db`)
- `WEAVER_API` — server URL the CLI talks to (default `http://127.0.0.1:7878`)
