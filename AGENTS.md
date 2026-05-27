# AGENTS.md

Engineer-facing notes for hacking on weaver itself. For user-facing docs read
[README.md](README.md); for the prompt the in-workspace agent sees, read
[primer.md](primer.md).

## Mental model

weaver = **server + thin CLI client + Vue SPA**, all talking over a local
HTTP+SSE API. There is no peer-to-peer logic and no task DAG: a **workspace** is
one git worktree + one tmux session running one agent, with a `goal` and an
evolving `description`. Workspaces are independent.

```
weaver CLI ──HTTP──▶ weaver server (axum, 127.0.0.1:7878)
                       ├─ SQLite (~/.weaver/weaver.db)
                       ├─ tmux + git subprocess wrappers
                       ├─ agent launcher + headless summarizer
                       └─ background monitor (status, orphan detection)
Claude Code hooks ──HTTP──▶ server
Vue SPA ──REST + SSE──▶ server
```

The server owns all state. The CLI is a thin client over `weaver::client`. The
only CLI command that *doesn't* go through HTTP is `weaver attach`, which
`exec`s `tmux attach`. Add new functionality as a REST endpoint first; the CLI
and SPA both consume it.

## Layout

| Path | What's in it |
|---|---|
| `src/bin/weaver.rs` | CLI: clap subcommands, dispatches into `client` |
| `src/client.rs` | HTTP client used by the CLI |
| `src/server.rs` | Server bootstrap (bind, write `server.json`, spawn bg tasks) |
| `src/web.rs` | axum routes, request/response types, SSE — **the API surface** |
| `src/workspace.rs` | `Workspace` model + sqlx queries |
| `src/db.rs` | Pool setup + schema (inline `CREATE TABLE IF NOT EXISTS`) |
| `src/config.rs` | Settings registry (label, type, default per key) |
| `src/agent.rs` | Launching agents into tmux, headless summary command |
| `src/tmux.rs` | `tmux new-session / send-keys / capture-pane / kill-session` |
| `src/git.rs` | `git worktree`, `merge-base`, diff, merge |
| `src/github.rs` | `gh` CLI shell-out for issue seeding |
| `src/monitor.rs` | Status detection + orphan marking |
| `src/summary.rs` | Background summarizer loop |
| `src/events.rs` | In-process broadcast bus that feeds SSE |
| `src/endpoint.rs` | `WEAVER_API` / default addr resolution (shared CLI + server) |
| `src/repo.rs` | Current-repo / current-workspace detection from `cwd` |
| `frontend/` | Vue 3 SPA, rspack, Tailwind. Single `api.ts`, views in `views/` |
| `static/dist/` | Build output (committed placeholder, real build overwrites) |
| `tests/integration.rs` | Spins a real server + git repo; needs `git` + `tmux` |
| `e2e/` | Playwright; talks to a real server. Separate `package.json` |
| `build.rs` | Runs `npm run build` in `frontend/`. Honors `WEAVER_SKIP_FRONTEND` |

## Build & test

```sh
cargo build                           # also runs `npm run build` in frontend/
WEAVER_SKIP_FRONTEND=1 cargo build    # backend only — fastest iteration
WEAVER_SKIP_FRONTEND=1 cargo test     # unit + integration; needs git & tmux
cd frontend && npm run dev            # live-reloading SPA against a running server
cd e2e && npm test                    # Playwright suite
```

The integration test shells out to real `git` and `tmux`. If it hangs, look for
stray `weaver-test-*` tmux sessions.

## Storage & state

- **SQLite** at `$WEAVER_HOME/weaver.db` (default `~/.weaver/weaver.db`). Schema
  is `CREATE TABLE IF NOT EXISTS` in `src/db.rs` — additive only, no migration
  framework. Add columns with `ALTER TABLE ... ADD COLUMN` guarded against the
  existing-column error.
- **`server.json`** in `$WEAVER_HOME`: pid + bound addr, written when the
  listener comes up. Clients use it to find the server when `WEAVER_API` is
  unset.
- **Settings** live in a `settings` table; each key is declared in
  `config::registry()` with label/help/type/default. Never written ⇒ default.
- **Worktrees** live under `<repo>/.worktrees/<slug>` on the branch
  `weaver/<slug>` (unless `--branch` reused an existing branch).

## Conventions

- **API-first.** New features land as a REST endpoint in `web.rs` and a
  client method in `client.rs`; the CLI and SPA are both consumers. Don't put
  business logic in `bin/weaver.rs` or in the Vue layer.
- **Errors:** server returns `AppError` (status + message + optional
  `details` map of per-field reasons); CLI uses `anyhow` and prints
  `error: {e:#}`.
- **Async:** tokio everywhere. Long-running subprocesses (tmux, git, gh, the
  agent) go through `tokio::process::Command`.
- **Events:** state changes that the SPA needs to see push through `EventBus`
  in `events.rs`; SSE handler in `web.rs` fans them out.
- **No tracking-branch state:** the server can be killed and restarted at any
  time. tmux sessions and worktrees survive; "orphaned" is a first-class
  status, recovered via `adopt`.

## Status detection

Two paths, picked per agent kind:

1. **Claude Code hooks** — installed into the worktree's
   `.claude/settings.local.json`, they POST `working` / `waiting` / `idle` to
   `/hook` (see `Cmd::Hook` in the CLI, dispatched by Claude). On `waiting`,
   the server snapshots the tmux pane into `pending_prompt`.
2. **Tmux stillness** — for non-Claude agents, the monitor diffs pane captures
   over time.

Orphan detection is independent of either: if `tmux has-session` says no, the
workspace becomes `orphaned` and is eligible for `adopt`.

## Frontend notes

- Vue 3 + `<script setup>` + Vue Router. Tailwind v4 via PostCSS.
- All server calls go through `frontend/src/api.ts`. Don't fetch inline in
  components.
- Types in `frontend/src/types.ts` mirror serde structs in `web.rs` — keep them
  in sync by hand (no codegen).
- No client-only state worth persisting; the server is the source of truth.
  Saved [[ui-built-on-rest-api]] applies: don't invent browser-local features
  that the CLI can't observe.

## Environment

| Var | Purpose | Default |
|---|---|---|
| `WEAVER_HOME` | state directory | `~/.weaver` |
| `WEAVER_DB` | sqlite path | `$WEAVER_HOME/weaver.db` |
| `WEAVER_API` | server URL (both sides — server binds here, CLI talks here) | `http://127.0.0.1:7878` |
| `WEAVER_SKIP_FRONTEND` | skip `npm run build` in `build.rs` | unset |
| `RUST_LOG` / `EnvFilter` | tracing filter | `weaver=info,tower_http=warn` |
