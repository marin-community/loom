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
weaver summary <id>                   # force a state summary now
weaver merge <id>                     # merge the branch into its base
weaver rm <id>                        # remove the worktree + tmux session
weaver open                           # open the web UI
```

Run inside a worktree, agents report progress with:

```sh
weaver goal                           # print the workspace goal
weaver description "wired up routes"  # set the current-state description
weaver note "blocked on the DB schema"
```

`weaver new --issue 123` seeds the goal/description from a GitHub issue (via the
`gh` CLI).

## Status detection

claude-backed workspaces report status via Claude Code hooks installed into
`.claude/settings.local.json` (`working` / `waiting` / `idle`). Other agents
fall back to tmux screen-stillness detection.

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
