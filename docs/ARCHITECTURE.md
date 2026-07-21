# Architecture

Deep reference for weaver's internals. [AGENTS.md](../AGENTS.md) is the short
how-to-work guide and links here; this file is for when you need the full map.

## Mental model

weaver ships **two binaries** over **loom's REST API**:

- **`weaver`** — the **agent-facing CLI**: a thin HTTP client (`weaver-api::Client`)
  of `loom`, resolving "the current branch" solely from `$WEAVER_BRANCH` (set by
  loom for every session it launches — there is no git-cwd fallback). It carries
  no sqlite driver; `reqwest` (via `weaver-api`) is its only network dependency.
  Agents call it to read and update the `goal` artifact, report status, add
  issues, set tags, and emit hook events. It **requires a reachable `loom server run`** —
  every command fails with a friendly error if the server can't be reached.
- **`loom`** — the **orchestrator**: the REST + SSE server, the Vue web UI, the
  per-branch terminal supervisor + agent process (via the `sessions` table), the
  background monitor, and the `git worktree` shell-outs. It is the only process
  that opens the sqlite database directly.

```
weaver CLI ──HTTP (REST)──▶ loom server run
                                │
                                ├─ sqlite ─▶ ~/.weaver/weaver.db
                                ├─ axum REST + SSE
                                ├─ terminal + git wrap.
                                ├─ agent launcher
                                ├─ monitor (consumes
                                │   `events` rows that
                                │   `weaver hook` posted)
                                └─ Vue SPA ──REST + SSE──▶ (browser)
```

Only `loom` opens the sqlite file directly; `weaver` reaches the same state
over HTTP. The monitor watches the `events` table for new `hook` rows —
`weaver hook` posts them via `POST /api/branches/{key}/events`, same as every
other `weaver` subcommand.

## Module layout

| Path | What's in it |
|---|---|
| `crates/weaver-core/` | lib: `branches`, `issues`, `events`, `db`, `migrations` (ordered SQL + `schema_migrations` indicator), `git`, `config`, `artifacts` (versioned documents), `repo_config` (`.weaver/config.toml`), `transcript` (agent conversation logs: raw → iris format → markdown), agent helpers. Pure logic; used by `loom` for DB access, and by `weaver` only for the DB-free pieces (`transcript`, `tags` constants/validators, the agent primer). |
| `crates/weaver-api/` | typed loom REST client + DTOs (`Client`, `*View`/`*Req` types, `endpoint::default_client()` for resolving `$WEAVER_API`/`$LOOM_TOKEN`). Zero server deps (no `axum`, no sqlite driver) — the one cross-process seam `weaver` links against instead of `weaver-core`'s DB layer. |
| `crates/smartdoc/` | the markdown-convention layer: parse references (`#N`, `artifact:<name>`), project live status into the render. Dependency-free of weaver. See [artifacts.md](artifacts.md). |
| `crates/weaver/src/bin/weaver.rs` | the slim agent-facing CLI (`summary`, `readme`, `status` [read or set level + message], `tag` [`set`/`rm`/`ls` a branch tag], `issue …`, `where`, `log`, `chatlog` [render the agent's conversation transcript], `hook`, `config` [read-only: `get`/`ls`; writes go through `loom config set` or the settings pane]) — every command drives `weaver-api::Client` over HTTP; none touch sqlite |
| `crates/loom/src/web.rs` | axum routes, request/response types, SSE — **the API surface** (incl. the auth middleware + login/token/user handlers) |
| `crates/loom/src/auth.rs` | authentication core: token/password crypto, the `users`/`api_tokens`/`auth_sessions` tables, the machine-local token, and the GitHub OAuth calls. `axum`-free so it unit-tests directly |
| `crates/loom/src/server.rs` | bind, write `server.json`, spawn bg tasks |
| `crates/loom/src/monitor.rs` | status detection, orphan marking, hook-event consumer, and the shared lifecycle-promotion path (`promote_lifecycle`) both the terminal hook consumer and the ACP turn-boundary driver (`record_acp_lifecycle`) run through |
| `crates/loom/src/watch.rs` | the watch engine: cron timer + event dispatcher + the round executor (the script subprocess executor every program runs on) |
| `crates/loom/src/builtins.rs` | the builtin watch program registry; the script programs are real Python files in `crates/loom/watches/`, embedded into the binary |
| `python/weaver-loom/` | the pure-Python layer over the loom REST API (`weaver_loom`: client + watch round context); stdlib-only, uv-buildable, vendored onto every script's `PYTHONPATH` by the engine; server-free contract tests in `tests/` (`uv run pytest`, CI's `python-binding` job) |
| `crates/loom/src/agent.rs` | launching agents: resolving the execution `protocol`, launching a `terminal` agent into a per-session PTY (installing its `.claude/settings.local.json` hooks) or building an `acp` launch (`build_acp_launch` — the adapter command, `_meta` options, and goal), plus the one-shot headless agent behind `POST /api/agent/oneshot` |
| `crates/loom/src/session.rs` | `Session` row + sqlx queries |
| `crates/loom/src/chatlog.rs` | conversation log: capture at archive (write the iris `chat.json` + rendered `chat.md` under `session.log_dir`) and serve it for the Conversation tab (`conversation()` — a terminal session's live transcript, an acp session's chat journal mapped to iris (`journal_to_log`), else the capture) |
| `crates/loom/src/backend.rs` | the terminal-management seam: every programmatic terminal op (create/has/capture/send/kill/list) drives the session's `tapestry` supervisor. Also the ACP transport seam — `new_relay_session`/`subscribe_relay`/`relay_write`/`relay_ack` drive a session's tapestry **relay** supervisor (a durable JSON-RPC frame spool) |
| `crates/tapestry/` | the per-session detached supervisor that outlives loom. Two modes: a **terminal** (PTY + vt100 screen emulator + unix control socket, streaming raw PTY bytes so an attached xterm owns its own scrollback/search), and a **relay** (a headless stdio subprocess whose stdout is split into newline-delimited frames, spooled with monotonic seqs, and replayed to a subscriber from any cursor — the durable transport under `loom::acp`) |
| `crates/loom/src/terminal.rs` | WebSocket ⇄ live-terminal bridge: xterm.js ⇄ the tapestry session socket |
| `crates/loom/src/acp/` | the [Agent Client Protocol](https://agentclientprotocol.com) client: one `tokio` task per `protocol='acp'` session drives a headless adapter subprocess (under a tapestry relay) over JSON-RPC 2.0 — consolidating streaming `session/update`s into journal blocks, block-boundary acking the relay spool, running the turn state machine, and answering permission requests. `start`/`attach` register a task into the `AppState.acp` registry the `/chat`, `/prompt`, `/permissions`, `/mode`, `/interrupt` routes drive. `acp/wire.rs` holds the JSON-RPC line codec + serde types |
| `crates/loom/src/chat.rs` | the ACP **chat journal**: the durable, block-structured (`chat_blocks`, one row per `(session_id, turn, seq)`) conversation record `loom::acp` writes idempotently and the `/chat` routes read |
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
sockets) and a throwaway git repo (see `e2e/fixtures/weaver.ts`). Sessions launch
under a deterministic, command-less custom agent (a bare login shell) the fixture
seeds as `shell`, so tests never spawn a real agent CLI. The per-test `weaver` fixture wipes every
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
  opened only by `loom` — `weaver` reaches it over HTTP. WAL mode handles
  concurrency among loom's own connections.
  - Core tables: `branches`, `issues`, `events`, `settings`.
  - Loom tables (`crates/loom/src/db.rs`): `sessions` (`origin` — the channel
    it was created through: `user`/`agent`/`github`/`slack`/`watch`/`actions`/
    `ops`, stamped server-side at create; `class` — `interactive`/`automation`,
    gating default-list visibility, see [Status & tags](#status--tags). A
    request may set `class` explicitly; otherwise `watch`/`actions`/`ops`
    origins default to `automation` while `github`/`slack` stay `interactive` —
    a person asked for those sessions and expects to find them on the board;
    `turn_count` — incremented on each `working` lifecycle edge;
    `tracking_issue_id` — the weaver issue opened at create. One *active*
    session per branch is enforced by a partial unique index on `branch_id`
    where `status NOT IN ('done', 'error', 'archived')` — an archived session
    releases its branch slot, so relaunching a done/archived branch is never
    blocked by its predecessor), `recent_repos`,
    `branch_github` (per-branch PR snapshot), `chat_blocks` (the ACP
    [chat journal](#rest-api): one row per `(session_id, turn, seq)` block),
    and the auth tables `users` (the approved-operator allowlist, seeded with
    the owner), `api_tokens` (hashed bearer tokens), and `auth_sessions`
    (hashed login cookies). See [Authentication](#authentication). Loom-owned
    tables have their own ordered migration stream under
    `crates/loom/migrations/`, recorded in `loom_schema_migrations`. This is a
    separate stream because the core migrations run before loom creates its
    tables. A pre-stream loom database is adopted once by presence-based
    introspection, then stamped at the baseline version; do not add more
    error-message-driven `ALTER TABLE` probes.
  - **Schema migrations** (`weaver-core/src/migrations.rs`): ordered SQL files
    under `crates/weaver-core/migrations/` (`NNNN_name.sql`, embedded with
    `include_str!`), applied at startup and recorded in a `schema_migrations`
    indicator table so each runs once. Add a change as a new numbered file plus
    a row in `MIGRATIONS`; never edit one that has shipped. A pre-framework
    database is brought to the baseline by a one-time `legacy_bootstrap` on
    first run.

### Database ownership and the PostgreSQL seam

The schema has two owners even though both currently share one SQLite file:

- `weaver-core` owns the durable work ledger (`branches`, issues, tags,
  events, artifacts, discussions, watches) and `schema_migrations`. Its
  original baseline also physically creates `settings`; operator config is a
  loom concern, but moving that table is still future boundary work.
- `loom` owns host-local runtime, identity, integration, and agent-config
  tables (`sessions`, chat, users/tokens, repos, agent config) and
  `loom_schema_migrations`.

The target rule is to keep cross-owner links as identifiers in application
code. Two existing exceptions remain explicit split prerequisites: the
`sessions.branch_id` schema has a physical foreign key to `branches`, and the
issue-list view joins sessions to branches to apply automation visibility.
Remove those before putting the owners in different databases, and do not add
new cross-owner joins in the meantime.

`weaver-core::db` is the backend seam: callers use its `Db` and
`DbTransaction` aliases, time values are computed by Rust and bound, row
decoding uses `FromRow`, and new conflict clauses should use portable
`ON CONFLICT` forms. The implementation is still deliberately SQLite:
`Db = SqlitePool`, connection setup uses WAL/`BEGIN IMMEDIATE`, migrations use
SQLite introspection, and runtime queries use SQLite placeholders. These
changes make a future backend explicit; they do not claim PostgreSQL support.

The first useful shared-PostgreSQL move is the durable ledger, not host-local
sessions. Its main data-model gate is a logical repository identity: today
ledger rows key repos by absolute checkout paths, so two hosts would otherwise
create two unrelated histories for the same repository. After that change —
and after removing the FK/join and relocating `settings` noted above — a
PostgreSQL implementation of the database seam and a real PostgreSQL CI lane
can move the ledger while each runtime host keeps its sessions and chat in
local SQLite.
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
- **Which repo a session forks from** is either a local checkout (`CreateReq.cwd`
  — the server resolves its main worktree) or a **managed repo**
  (`CreateReq.repo`: a GitHub `owner/name` slug or clone URL). A managed repo is
  cloned into the repo store (`$WEAVER_REPOS_DIR`, default `$WEAVER_HOME/repos`,
  laid out `<root>/<owner>/<name>`) on first use and fetched thereafter, and the
  worktree is cut from that clone. Naming one on an authenticated create
  registers it in the `repos` table, so `loom launch --repo acme/widgets` works
  against a repo this machine has never checked out. That table doubles as the
  clone **allowlist** for the *unauthenticated* GitHub webhook, which resolves its
  own clone through `repo::resolve_clone` and refuses a repo that is not on it.

## REST API

All routes live under `/api`. The Vue SPA is the primary consumer.

| Method + path | What it does |
|---|---|
| `GET /api/health` | liveness probe |
| `GET /api/sessions` / `POST /api/sessions` | list / create sessions (list takes `archived` — default `false`, include torn-down sessions — and `automation` — default `false`, include automation-class sessions, otherwise hidden from the default fleet view; create takes optional `scratch: [{name, content_base64}]`, `parent_branch`, `protocol` (`terminal`\|`acp`, an opt-in override of the agent's declared default), and `mode` (the initial ACP permission posture; omitted uses `agent.mode`); opens a tracking issue and returns its id as `tracking_issue`) |
| `GET PATCH DELETE /api/sessions/{id}` | session CRUD (status, title, goal, description) |
| `PUT DELETE /api/sessions/{id}/tags/{key}` | set (upsert) / clear a branch tag — the well-known `attention` and `triage` keys plus any free-form key |
| `GET /api/sessions/{id}/url` | the session's dashboard URL as `{url}`, built from the externally-visible origin (`auth.base_url`, else the request's own Host) — what `loom session url` prints, so an agent can link a PR back to its session without inventing a loopback link |
| `POST /api/sessions/{id}/{archive,adopt}` | actions |
| `POST /api/sessions/{id}/handoff` | replace an idle ACP session's agent runtime/profile while preserving its loom session, worktree, branch, and canonical chat journal; the new provider receives a bounded dialogue replay and the journal records a compact handoff boundary |
| `POST /api/sessions/{id}/github` | re-poll the branch's GitHub PR now and return the updated session |
| `GET POST DELETE /api/sessions/{id}/scratch` | list / drop / remove worktree `scratch/` reference files |
| `PUT /api/sessions/{id}/file?path=…` | write raw bytes to a worktree file (the editor save primitive) |
| `GET /api/sessions/{id}/artifacts` | list the branch's [artifacts](artifacts.md) plus repo-shared ones |
| `GET PUT /api/sessions/{id}/artifacts/{name}` | read content + projected refs (`rev=N` for a revision) / write a user edit as a new revision |
| `GET /api/sessions/{id}/{diff,log,events}` | reads + SSE stream |
| `GET /api/sessions/{id}/conversation` | the agent conversation as a normalized iris log (live transcript, else the archive capture); 404 when there is none — backs the Conversation tab |
| `GET /api/sessions/{id}/terminal` | WebSocket: xterm.js ⇄ the session's tapestry PTY (the interaction surface) |
| `POST /api/sessions/{id}/send` | type `{text}` into the agent's terminal (`submit`, default true, follows it with Enter); for an `acp` session it delegates to the prompt path (steering a supported live turn, otherwise queueing), keeping the same `nudge` audit |
| `POST /api/sessions/{id}/interrupt` | stop the current turn — a break (Escape) to the terminal for a `terminal` session, `session/cancel` for an `acp` one |
| `GET /api/sessions/{id}/preview?lines=N` | capture the screen as `{screen}`; `lines` adds scrollback above the visible screen (for an `acp` session, `{screen}` is the last `lines` journal blocks rendered as compact text) |
| `GET /api/sessions/{id}/chat` | `{blocks: [ChatBlockView], live_turn}` — the ACP session's journal snapshot (per-block conversation record) plus the in-flight turn, if any |
| `GET /api/sessions/{id}/chat/stream` | SSE tail of the live journal: `block` (a committed block), `delta` (a streaming message/thought chunk), `tool` (a live tool-call update), `turn` (started / ended) |
| `POST /api/sessions/{id}/prompt` | `{text}` → 202 `{queued, steered, turn}` — dispatch a user message as a `session/prompt`; a live turn uses the advertised codex-acp steering extension, with the durable next-turn queue as fallback |
| `DELETE /api/sessions/{id}/prompt` | atomically retract unseen next-turn feedback and return `{text}` for editing; 409 when the current ACP state has no queue available to retract |
| `POST /api/sessions/{id}/permissions/{request_id}` | `{option_id}` → answer an open permission request (200 / 404 unknown / 409 already resolved) |
| `PUT /api/sessions/{id}/mode` | `{mode_id}` → change the ACP session's permission mode (`session/set_mode`), journaled as a `mode_change` |
| `GET /api/branches` / `GET PATCH /api/branches/{id}` | list / inspect / edit tracked branches |
| `GET POST /api/branches/{id}/issues` | issues claimed by the branch / create one |
| `GET /api/issues?all=…` | the cross-repo issue board (every repo's issues; `all=true` includes closed, `automation=true` includes automation-class sessions' tracking issues, otherwise hidden) — what the loom Issues pane reads |
| `GET PATCH DELETE /api/issues/{id}` | per-issue CRUD |
| `PUT DELETE /api/issues/{id}/tags/{key}` | set (upsert) / clear a free-form issue label — quiet `(key, value)` pills, no loud `attention`/`triage` ladder |
| `GET POST /api/repos/issues?repo_root=…` | repo-wide board (`scope=repo\|backlog`) / create a backlog item |
| `GET /api/repos/recent` / `GET /api/repos/branches?cwd=…` | recent repos / branches in a repo |
| `GET /api/agents` | first-class agent types, their advertised model/effort selectors, and their execution `protocol` (`terminal`\|`acp`) — backs the create-session form and server-side validation |
| `GET PATCH /api/settings` | settings registry |
| `GET /api/auth/me` | caller identity + sign-in methods (public; never 401s) |
| `POST /api/auth/login` / `POST /api/auth/logout` | username/password login / drop the session (public) |
| `GET /api/auth/github/{login,callback}` | the GitHub OAuth dance (public) |
| `GET POST /api/auth/tokens` / `DELETE /api/auth/tokens/{id}` | list / mint / revoke API tokens |
| `POST /api/auth/password` | set the caller's own password |
| `GET POST /api/auth/users` / `DELETE /api/auth/users/{username}` | the approved-operator allowlist |
| `GET PUT /api/auth/github/config` | the GitHub OAuth app config (secret write-only) |
| `GET POST /api/watches` / `GET PATCH DELETE /api/watches/{id}` | watch CRUD (see [Watches](#watches)) |
| `GET /api/watches/programs` | the builtin program registry: titles, suggested defaults, read-only script sources |
| `POST /api/watches/{id}/run` / `GET /api/watches/{id}/runs` | fire a round now (`{dry_run}` stubs mutations) / the round-history audit |

`SessionView` (`/api/sessions[/...]`) returns session-specific fields
top-level (`id`, `status`, `work_dir`, `term_session`, `agent_kind`, `model`,
`effort`, `pending_prompt`, `github_repo`, `github_issue` (the `repo` + `number`
linked on the session's tracking issue), `last_activity_at`,
`created_at`, `updated_at`, `parent_id`, `protocol` (`terminal` or `acp`),
`acp_session_id`, `current_mode`, `usage` (`{used, size}` context window, from
the journal's latest `usage` block), `origin` (the channel that created it:
`user`/`agent`/`github`/`slack`/`watch`/`actions`/`ops`), `class`
(`interactive`/`automation`), `turn_count` (incremented on each `working`
lifecycle edge), and `tracking_issue` (the weaver issue opened at create;
populated on every read, not just the create response)) plus a nested
`branch: BranchView`
(`id`, `name`, `title`, `goal`, `description`, `tags`,
`repo_root`, `branch`, `base_branch`, `created_at`, `updated_at`,
`open_issue_count`, `github`).

`BranchView::tags` is the branch's tag list — each a `TagView`
(`key`, `value`, `note`, `set_by`, `set_at`). A tag is a single-valued
`(key, value)` annotation on a branch; the well-known keys are `attention` (the
agent's self-report) and `triage` (a watch's assessment), and any other
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
uniformly. For a `terminal` session each requires a live terminal (else 409). An
`acp` session has no PTY, so the same verbs map onto the protocol — keeping the
CLI (`loom session {send,break,preview}`) and its `nudge` audit uniform across
backends: `send` delegates to the prompt path (steered when supported, otherwise
queued while a turn is live), `interrupt` is a `session/cancel`, and `preview` renders the
last journal blocks as compact plain text instead of a vt100 screen capture.

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
  at any time. Terminal *and* relay supervisors and worktrees survive (the
  supervisor is a detached process, independent of `loom server run`); "orphaned"
  is a first-class status, recovered via `loom session adopt` (or the Adopt button
  in the UI). On startup loom re-attaches every live-relay ACP session so its
  journal keeps flowing; adopt re-attaches when the relay outlived a crashed task,
  or respawns the adapter and reopens the conversation via `session/load` (falling
  back to a fresh session re-oriented from the goal) when the relay is gone too.

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

**Protocol axis.** Every agent declares an execution `protocol` — `terminal`
(the agent runs in a PTY loom drives by keystroke) or `acp` (a headless adapter
loom drives over the [Agent Client Protocol](https://agentclientprotocol.com)).
The builtins are `terminal`; a custom agent carries its own `custom_agents.protocol`
column. A create may **override** to `acp` where the agent allows it (Claude opts
in; Codex rejects it, as `codex-acp` is a later phase), and the resolved protocol
is stamped on the `sessions` row at create, immutable thereafter. The row's
protocol — not the agent kind — is what every downstream path (launch, lifecycle,
drive routes, adopt, archive) branches on.

**Lifecycle** is driven by that protocol. A `terminal` session's lifecycle rides
Claude Code's hooks, so that path merges a `hooks` block into the worktree's
`.claude/settings.local.json` (see `loom::agent::install_hooks` and
`weaver_core::agent::hooks_json`); hookless terminal agents — Codex, and any
custom agent whose `reports_status` is off — start `running` immediately:

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

An **`acp` session drives the same lifecycle from the protocol's turn boundaries**
rather than hooks: the acp task calls `monitor::record_acp_lifecycle` at turn
start (`working`) and turn end (`idle`), which records the very `hook` event row
`weaver hook` would and then runs the shared `promote_lifecycle` path — so the
status lift and tag mutations live in exactly one place across both backends. The
monitor's `apply_hook` therefore *ignores* an acp session (a stray work-cycle hook
a user's own settings might still fire must not move it), and the acp task is the
sole driver. Claude-over-ACP installs **only** the `SessionStart` primer hook (the
`additionalContext` injection is still wanted); the work-cycle hooks and the
launch-gate seed are redundant under ACP, where the protocol's turn edges and the
`bypassPermissions` posture replace them.

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
status <level> ["<message>"]`. That calls `POST /api/branches/{key}/status`,
which writes the tag (and, when a message is given, the `description`) and
records a `tag` event the monitor re-broadcasts over SSE, atomically in one
request — `ok` clears the tag, the two loud levels upsert it. The message rides
the event as its `note`, so the event log carries the full **status trail** —
the progress log the dashboard's activity feed renders, `weaver log` prints,
and a GitHub-wired session mirrors publicly (see [GitHub
integration](#github-integration)). A bare `weaver
status <level>` changes only the level and keeps the last message. Last write
wins, so an explicit declaration overrides the hook-inferred default. The
general `weaver tag set|rm|ls` group writes any key the same way, over the
branch-scoped `PUT`/`DELETE /api/branches/{key}/tags/{key}` routes; the
session-scoped `PUT`/`DELETE /api/sessions/{id}/tags/{key}` routes serve the
UI and the [watch](plans/watches.md). The builtin status watch, when a
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
merge-archive poller get it). For a `terminal` session the agent's transcript
lives outside the worktree — Claude Code under `~/.claude/projects/<munged-cwd>/`,
Codex under `~/.codex/sessions/` — so it survives the worktree removal; capture
locates it and normalizes it through `weaver_core::transcript`. An `acp` session
has no external JSONL: its transcript **is** loom's own chat journal, mapped to
the same iris shape (`chatlog::journal_to_log`). Either way capture produces the
same pipeline output (raw → **iris format** → a rendered markdown log) and writes
`chat.json` (iris) + `chat.md` under
`<session.log_dir>/<branch>/` (`session.log_dir` defaults to
`~/.iris/logs/sessions`). It is best-effort: a missing or unreadable transcript
is a logged warning, never a failed archive. The same conversion/render pipeline
backs `weaver chatlog`, which renders the current worktree's (or a named file's)
transcript on demand.

The dashboard surfaces this as a **Conversation tab** on the session detail,
backed by `GET /api/sessions/{id}/conversation` (`chatlog::conversation` → for a
`terminal` session the live transcript when present, else the archived
`chat.json`; for an `acp` session the chat journal mapped to iris live, so the
existing tab keeps working before the SPA rewires onto `/chat`). The Vue viewer
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

**Automation lifecycle.** A `class = automation` session — every session not
launched interactively by a human, excluding a watch's own warm sessions —
carries a turn cap (`automation.turn_cap`, default `100`, `0` disables)
counted by `sessions.turn_count`. Exceeding the cap raises a loud `blocked`
attention tag and the ACP driver refuses to start a new turn. The monitor also
reaps automation sessions: one is archived once its `tracking_issue_id`
closes, or after `automation.idle_archive_secs` (default `28800`, `0`
disables) of inactivity — both guarded by a no-live-turn check and a grace
period, so a session mid-turn or only just gone quiet is never torn down out
from under it. The `automation.*` settings live in
`weaver-core::config::registry()` under the **Automation** group.

## GitHub integration

When the `gh` CLI is installed and authenticated, loom keeps a per-branch
pull-request snapshot alongside the session. A second background loop
(`github::poll`, sibling of the monitor, spawned in `server::serve`) ticks every
30s and, for each active session, runs `gh pr list --head <branch> …` from the
repo root. `<branch>` is the worktree's live HEAD (falling back to loom's stored
branch identity when the worktree cannot be read), so an agent that switches or
renames its branch before opening a PR is still discovered. The result — PR
number, URL, state (`OPEN`/`CLOSED`/`MERGED`), draft
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

The session header always renders PR and issue association pills, including an
empty state. The PR editor can pin an explicit number or return to live-branch
discovery through `PUT` / `DELETE /api/sessions/{id}/github`; the issue editor
patches the GitHub link on the session's weaver tracking issue, which remains
the source of truth for that association.

**Archive on merge.** When a poll finds a branch's PR has merged and
`github.archive_on_merge` is on (the default), loom archives the session
automatically — the same teardown as the Archive button (`web::archive`, shared
code): the terminal killed, worktree removed, branch and weaver history kept. The
worktree is removed with `--force`, so any uncommitted work in it is discarded;
a merged PR is taken to mean the workstream is done. Turn the behaviour off with
`loom config set github.archive_on_merge false` (or in the settings pane).
Both settings live in `weaver-core::config::registry()` under the **GitHub**
group.

`gh`-touching logic lives in `crate::github`: `fetch_pr` (the shell-out +
JSON parse + check rollup), `refresh` (fetch → store → announce → maybe
archive, behind both the poller and the refresh endpoint), and `poll` (the
loop). The merge-archive decision is split into `apply_snapshot` so it is
testable without invoking `gh`.

**The status card.** A branch carrying the quiet `github` tag
(`owner/name#number` — stamped by the `@loom` trigger, or set by hand with
`weaver tag set github …`, format-validated at set time) mirrors its status
trail onto that GitHub thread: `github::sync_status_comment`, spawned detached
by the status endpoint and by artifact writes, renders one comment — the
session link, links to the branch's artifacts, and the trail of the agent's
own `attention` events since wiring — and edits it in place through the
trigger's `GithubApi` gateway (`post_issue_comment` returns the comment id;
`update_issue_comment` PATCHes it, reporting a deleted comment as `Ok(false)`
so the card is reposted, while transient errors retry). A process-wide lock
serializes syncs so racing writes can't double-post. The comment id lives in
the machine-owned `github.status_comment` tag (note = the wiring it belongs
to, so re-pointing the `github` tag posts fresh instead of editing the old
thread); it and `github.linked` are refused by the tag-set routes and hidden
from the dashboard's pill row. See
[github-trigger.md "The status card"](github-trigger.md#the-status-card).

## Authentication

Authentication is a **loom-only** concern — `weaver` authenticates like any
other REST client, sending `$LOOM_TOKEN` as a bearer token when set (falling
back to loom's machine-local token). It lets loom be exposed off the loopback
interface (so the dashboard and the API are reachable without an SSH tunnel)
while gating who may drive the fleet. The core (crypto, the tables, the
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
  `loom` calls, and watch scripts working with zero configuration. To get
  the peer address, the server runs `into_make_service_with_connect_info`; the
  decision uses the real socket peer, **never** a forwarded header.

A request that resolves to none of these gets `401`; the SPA's router guard
turns that into the login screen.

**The allowlist.** `users` rows are the approved operators. A fresh database is
seeded with one owner — whichever GitHub login `LOOM_OWNER_GITHUB` names at
first run. There is no default: leave it unset and no owner row is seeded at
all, so GitHub login has no `users` row to match until it's set (fail closed,
rather than seed a real maintainer login onto an internet-facing deploy).
GitHub login only succeeds for a login that matches a `users` row; an unknown
identity is authenticated by GitHub but rejected by loom. A user may have a
`github_login`, a `password_hash` (argon2), or both. All approved users are
equal — there is no role hierarchy, matching the single-operator scale.

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
same-host subprocesses (the agent's terminal, watch scripts) — and the `loom`
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

## Watches

A **watch** is a periodic / triggered program over the fleet: it
wakes on a trigger (a cron tick or a session event), surveys the sessions in
scope, and acts within an explicit capability set. The design of record is
[docs/plans/watches.md](plans/watches.md). The engine
(`loom::watch`, spawned in `server::serve`, self-gated on the
`watch.enabled` setting) runs each **round** under non-optional guardrails
— no-overlap, cooldown, a wall-clock timeout, no-recursion — and records it in
`watch_runs`, the audit trail the panel's round history renders.

A round runs the **program** the watch names:

- **Builtin scripts** — real Python files in `crates/loom/watches/`,
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
  the actual archive is still `github.archive_on_merge`, above. The Watch
  panel and `loom watch programs` list the registry; script sources
  render read-only (they ship with the binary).
- **A custom program file** — an absolute path, conventionally
  `~/.weaver/watches/<name>.py` (`loom watch new` scaffolds one).

Builtin scripts and custom files run on one executor: an env-stripped
subprocess that reaches the fleet only through the loom REST API — everything
loom can do is an HTTP route (including one-shot agent judgement, at
`POST /api/agent/oneshot`), and Python is purely a convenience layer on top.
There is deliberately no privileged in-Rust program shape: a builtin sees
exactly the API a custom program sees.
The contract: `$WEAVER_API` carries the daemon's base URL, `$WEAVER_WATCH`
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
| `WEAVER_DB` | sqlite path, read only by `loom` | `$WEAVER_HOME/weaver.db` |
| `WEAVER_API` | loom URL (both sides — server binds, `weaver`/`loom` CLI clients talk) | `http://127.0.0.1:7878` |
| `WEAVER_BRANCH` | the current branch key, set by `loom session launch` in the worktree — the only source `weaver` uses; unset, every `weaver` command fails with a friendly error | — |
| `LOOM_TOKEN` | bearer token the `weaver`/`loom` CLIs and automation send; falls back to the machine-local token file on the same host | — |
| `LOOM_OWNER_GITHUB` | GitHub login seeded as the owner on a fresh database; unset seeds no owner at all | — |
| `LOOM_GITHUB_CLIENT_ID` / `LOOM_GITHUB_CLIENT_SECRET` | GitHub OAuth app credentials (override the settings-stored values) | — |
| `WEAVER_TAPESTRY_DIR` | directory holding tapestry's per-session control sockets | `$WEAVER_HOME/sock` |
| `WEAVER_TAPESTRY_BIN` | the `tapestry` supervisor binary loom re-execs (else a sibling of `loom`); set by the tests | sibling of `loom` |
| `WEAVER_WATCH_AGENT_CMD` | the one-shot headless agent command behind `POST /api/agent/oneshot` (judgement calls) | `claude -p` |
| `RUST_LOG` / `EnvFilter` | tracing filter | `loom=info,weaver_core=info,tower_http=warn` |
