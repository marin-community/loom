# Architecture

Deep reference for Loom's internals. [AGENTS.md](../AGENTS.md) is the short
how-to-work guide and links here; this file is for when you need the full map.

## Mental model

Loom exposes one registered operation model through several adapters:

- **`loom`** — the **orchestrator**: the REST + SSE server, the Vue web UI, the
  per-session detached Tapestry runtime supervisor + agent process (via the
  `sessions` table), the background monitor, and the `git worktree` shell-outs.
  It is the only process that opens the sqlite database directly. Its CLI, MCP
  servers, and browser are thin clients of the same registered REST operations.
```
loom CLI / MCP ──HTTP (REST)──▶ loom server run
                                │
                                ├─ sqlite ─▶ ~/.weaver/weaver.db
                                ├─ axum REST + SSE
                                ├─ terminal + git wrap.
                                ├─ agent launcher
                                ├─ monitor (consumes
                                │   `events` rows that
                                │   `loom hook` posted)
                                └─ Vue SPA ──REST + SSE──▶ (browser)
```

Only the server opens the sqlite file directly. The monitor watches the
`events` table for new `hook` rows; `loom hook` posts them through the same REST
boundary as other commands.

## Module layout

[crate-layering.md](crate-layering.md) covers the crate split itself: why the
loom crates are cut where they are, the module cycles that constrain the cut,
and the rule for placing a new module.

| Path | What's in it |
|---|---|
| `crates/weaver-core/` | legacy-named internal domain crate: branches, issues, events, db/migrations, git, config, artifacts, reviews, repository configuration, transcripts, and agent helpers. It is not a command or product surface. |
| `crates/weaver-api/` | legacy-named typed Loom REST client + DTOs (`Client`, `*View`/`*Req` types, `endpoint::default_client()` for resolving `$WEAVER_API`/`$LOOM_TOKEN`). Zero server deps (no `axum`, no sqlite driver); it is the cross-process API seam. |
| `crates/smartdoc/` | the markdown-convention layer: parse references (`#N`, `artifact:<name>`), project live status into the render. Dependency-free of weaver. See [artifacts.md](artifacts.md). |
| `crates/loom/src/agent_cli.rs` | HTTP-only implementations behind Loom's session, channel, artifact, issue, status, and settings commands |
| `crates/loom/src/web/` | axum routes, request/response types, SSE — **the API surface** (incl. the auth middleware + login/token/user handlers) |
| `crates/loom-ctx/` | leaf utilities and `Ctx` (the storage handle, event bus and server address every layer above threads through). No loom dependency of its own; knows nothing about sessions |
| `crates/loom-store/` | durable records and storage operations: sessions, chat, channels, layout, runs, history, and the profile record |
| `crates/loom-agent/` | agent mechanisms: ACP, builtin/custom runtimes, and trusted MCP adapters |
| `crates/loom-policy/` | launch and access policy: profiles, authentication, automation, custom MCP administration, and composed database initialization |
| `crates/loom-core/` | shared engine operations: launch resolution, shells, and detached-session ownership reconciliation |
| `crates/loom-editor/` | attaching a human to a live session through the terminal or embedded editor |
| `crates/loom-forge/` | GitHub, registered repositories, credentials, runtime lifecycle, and `AppState` |
| `crates/loom-launch/` | repository/worktree preparation, provisioning, metadata assistance, and handoff |
| `crates/loom-watch/` | status monitor, watch scheduler, builtin programs, and background maintenance |
| `crates/loom-deliver/` | Slack and submitted-review delivery |
| `crates/loom/src/lib.rs` | crate boundary: the HTTP adapter and CLI over the ten engine crates, re-exported here so `loom::session`, `loom::AppState` and friends resolve regardless of which crate defines them. The crate does not publicly re-export `weaver-core` storage/domain modules |
| `crates/loom-policy/src/auth.rs` | authentication core: token/password crypto, the `users`/`api_tokens`/`auth_sessions` tables, the machine-local token, and the GitHub OAuth calls. `axum`-free so it unit-tests directly |
| `crates/loom-ctx/src/client_context.rs` | named endpoint and credential resolution for the `loom` CLI: XDG user config, private credentials, and repository context selection |
| `crates/loom/src/server.rs` | bind, write `server.json`, spawn bg tasks |
| `crates/loom-watch/src/monitor.rs` | status detection, orphan marking, hook-event consumer, and the shared lifecycle-promotion path (`promote_lifecycle`) both the terminal hook consumer and the ACP turn-boundary driver (`record_acp_lifecycle`) run through |
| `crates/loom-watch/src/watch.rs` | the watch engine: cron timer + event dispatcher + the round executor (the script subprocess executor every program runs on) |
| `crates/loom-watch/src/builtins.rs` | the builtin watch program registry; the script programs are real Python files in `crates/loom-watch/watches/`, embedded into the binary |
| `python/weaver-loom/` | the pure-Python layer over the loom REST API (`weaver_loom`: client + watch round context); stdlib-only, uv-buildable, vendored onto every script's `PYTHONPATH` by the engine; server-free contract tests in `tests/` (`uv run pytest`, CI's `python-binding` job) |
| `crates/loom-agent/src/agent.rs` | `AgentManager` plus launch mapping: resolves registered runtimes, launches terminal agents, builds ACP launches, and runs transient ACP judgement prompts for handoff summaries and `POST /api/agent/oneshot` |
| `crates/loom-agent/src/mcp/` | trusted builtin MCP registry and stdio adapters: provider-neutral versioned capability sets, exact permission translation, and the fixed GitHub/messaging/self-history bridges |
| `crates/loom-policy/src/custom_mcp.rs` | operator-authored MCP definitions: grouped path identities, immutable sqlite revisions, bounded `uv` validation, and exact session-snapshot execution |
| `crates/loom-policy/src/profile.rs` | named launch policy, including provider-neutral `mcp_access` resolution and the restricted-profile trust boundary |
| `crates/loom-core/src/launch.rs` | canonical profile-template and override resolution for previews, creates, clones, and handoffs; returns the concrete private launch snapshot plus its transport-safe view |
| `crates/loom-launch/src/handoff.rs` | provider handoff orchestration: canonical/legacy target resolution, conversation continuity, lifecycle fencing, rollback, and replacement cleanup; depends on runtime/domain owners, never the REST adapter |
| `crates/loom-launch/src/provision.rs` | ordinary session provisioning: trusted actor attribution, canonical launch resolution, repository/worktree/setup lifecycle, stamped launch snapshots, tracking, recoverable launch-failure surfacing, and title generation; returns only the created `Session` + `Branch` domain facts |
| `crates/loom-ctx/src/scratch.rs` | shared Scratch validation and filesystem storage for launch-time attachments and live route mutations; Axum-free semantic errors keep transport mapping in `web/` |
| `crates/loom-store/src/session.rs` | `Session` row + sqlx queries |
| `crates/loom-store/src/channels.rs` | same-id session channels and custom communication contexts: atomic creation, append-only typed messages, per-subject subscriptions/read markers, lifecycle, and runtime-delivery receipts |
| `crates/loom-store/src/session_layout.rs` | durable Spaces → Groups → Sessions placement, defaults, ordering, optimistic mutation revisions, and revision-invalidation publication; independent of immutable provenance and launch policy |
| `crates/loom-core/src/session_manager.rs` | database-backed ownership reconciliation for detached agent/debug supervisors; removes Loom-namespaced runtimes without a live session or active launch-reservation owner |
| `crates/loom-deliver/src/review_delivery.rs` | submitted-review outbox and protected conversation-inbox delivery, including ACP claim fencing and terminal retry/rehome behavior |
| `crates/loom-launch/src/metadata_assist.rs` | bounded generated task-title and resumption-cue assistance on the session's ACP runtime, with privacy eligibility, source fences, and deterministic fallback |
| `crates/loom-store/src/chatlog.rs` | conversation log: capture at archive (write the iris `chat.json` + rendered `chat.md` under `session.log_dir`) and serve it for the Conversation tab (`conversation()` — a terminal session's live transcript, an acp session's chat journal mapped to iris (`journal_to_log`), else the capture) |
| `crates/loom-store/src/history.rs` | provider-neutral session-history records and bounded paging/literal search across the ACP journal or terminal Iris normalization; optional fields describe only source-supplied data |
| `crates/loom-ctx/src/backend.rs` | the terminal-management seam: every programmatic terminal op (create/has/capture/send/kill/list) drives the session's `tapestry` supervisor. Also the ACP transport seam — `new_relay_session`/`subscribe_relay`/`relay_write`/`relay_ack` drive a session's tapestry **relay** supervisor (a durable JSON-RPC frame spool) |
| `crates/tapestry/` | the per-session detached supervisor that outlives loom. Two modes: a **terminal** (PTY + vt100 screen emulator + unix control socket, streaming raw PTY bytes so an attached xterm owns its own scrollback/search), and a **relay** (a headless stdio subprocess whose stdout is split into newline-delimited frames, spooled with monotonic seqs, and replayed to a subscriber from any cursor — the durable transport under `loom::acp`) |
| `crates/loom-editor/src/terminal.rs` | WebSocket ⇄ live-terminal bridge: xterm.js ⇄ the tapestry session socket |
| `crates/loom-editor/src/ide.rs` | lazy per-session code-server ownership plus the authenticated same-origin HTTP/WebSocket reverse proxy used by the advanced editor panel |
| `crates/loom-agent/src/acp/` | the [Agent Client Protocol](https://agentclientprotocol.com) client: one `tokio` task per `protocol='acp'` session drives a headless adapter subprocess (under a tapestry relay) over JSON-RPC 2.0 — consolidating streaming `session/update`s into journal blocks, block-boundary acking the relay spool, running the turn state machine, and answering permission requests. `start`/`attach` register a task into the `AppState.acp` registry the `/chat`, `/prompt`, `/permissions`, `/mode`, `/interrupt` routes drive. `acp/wire.rs` holds the JSON-RPC line codec + serde types |
| `crates/loom-store/src/chat.rs` | the ACP **chat journal**: the durable, block-structured (`chat_blocks`, one row per `(session_id, turn, seq)`) conversation record `loom::acp` writes idempotently and the `/chat` routes read |
| `crates/loom-forge/src/github.rs` | `gh` CLI shell-out: issue seeding, PR opening, and the PR-status poll loop (snapshots each branch's PR; archives on merge) |
| `crates/loom/src/client.rs` | HTTP client used by the `loom` CLI to talk to its own daemon |
| `crates/loom/src/bin/loom.rs` | the orchestrator CLI (`server`, `sessions`, `ps`, `attach`, …) |
| `crates/loom/frontend/` | Vue 3 SPA, rspack, Tailwind. `api.ts` + views in `views/`; the visual rules live in [loom-ui.md](loom-ui.md) |
| `crates/loom/static/dist/` | Build output (placeholder; real build overwrites) |
| `crates/loom/tests/` | integration tests: `integration/` (server suites) + `hook_monitor.rs`; need `git` (they spawn `tapestry` supervisors, built by the same `cargo test`) |
| `e2e/` | Playwright; talks to a real `loom server run`. Separate `package.json` |
| `crates/loom/build.rs` | Builds the SPA into `static/dist` (npm + rspack); writes a placeholder when Node is unavailable |

## Build internals

`cargo build` builds the SPA into `static/dist` via `build.rs`; loom serves it
from there at runtime (`web::static_dir`). `rerun-if-changed` makes the SPA build
a no-op when no frontend source changed, so backend-only edits don't re-run
rspack; a Node-less checkout still builds the backend and serves a placeholder.
There is no skip flag.

Tests are proportional by layer. Rust and frontend unit tests own pure module
logic; `crates/loom/tests/integration/` proves cross-module wiring against an
isolated live server; Playwright owns browser journeys; Python package and
builtin-watch logic lives in pytest. `scripts/test-representative.sh` amortizes
one selected journey from each setup-heavy feature after running the unit
suites. Full workspace and Playwright suites remain the exhaustive CI gates.

`scripts/pre-commit.sh` is the deterministic local/CI gate: Rust formatting and
clippy plus frontend unit tests, typecheck, and Prettier when npm is available.
The separate agent lint-review policy and invocation live in
[`docs/lint.md`](lint.md) and the pull-request skill, not in the commit hook.

The integration tests shell out to real `git` and spawn `tapestry` terminal
supervisors (detached PTY processes). The harness kills its supervisors on drop;
if one hangs, look for stray `tapestry supervise` processes.

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
  opened only by `loom`; every external adapter reaches it over HTTP. WAL mode handles
  concurrency among loom's own connections.
  - Core tables: `branches`, `issues`, `events`, `settings`.
  - Loom tables (`crates/loom-store/src/db.rs`): `sessions` (`origin` — the channel
    it was created through: `user`/`agent`/`github`/`slack`/`watch`/`actions`/
    `ops`, stamped server-side at create; `class` — `interactive`/`automation`,
    retained as machine provenance and for policy rather than as a separate
    fleet surface. A
    request may set `class` explicitly; otherwise `watch`/`actions`/`ops`
    origins default to `automation` while `github`/`slack` stay `interactive` —
    a person asked for those sessions and expects to find them on the board;
    `turn_count` — incremented on each `working` lifecycle edge;
    `tracking_issue_id` — the optional explicitly claimed/imported compatibility
    work item. One *active*
    session per branch is enforced by a partial unique index on `branch_id`
    where `status NOT IN ('done', 'error', 'archived')` — an archived session
    releases its branch slot, so relaunching a done/archived branch is never
    blocked by its predecessor), `session_layout_state`, `session_spaces`,
    `session_groups`, `session_placements` (one canonical ordered placement per
    session), `session_placement_defaults` (origin/profile routing), and
    `user_session_group_state` (operator-local collapse preferences),
    `channels`, `channel_messages`, `channel_subscriptions`, and
    `channel_deliveries` (durable communication context, append-only stream,
    per-subject read/mode state, and separate runtime acceptance receipts),
    `recent_repos`,
    `slack_routes` (Slack threads an automation delivery pointed at a branch,
    keyed on the thread: many alert threads may route to one operator session,
    which is why this is not the single-valued `slack` branch tag),
    `branch_github` (per-branch PR snapshot), `chat_blocks` (the ACP
    [chat journal](#rest-api): one row per `(session_id, turn, seq)` block),
    `session_acp_metadata` (the latest provider-advertised composer controls,
    retained when its live task exits or Loom restarts),
    `session_metadata_assistance` (per-session generated-title enable/status and
    the cached resumption cue keyed by a bounded conversation/content +
    immutable-artifact fingerprint),
    and the auth tables `users` (the approved-operator allowlist, seeded with
    the owner), `api_tokens` (hashed bearer tokens), and `auth_sessions`
    (hashed login cookies). See [Authentication](#authentication). Loom-owned
    tables have their own ordered migration stream under
    `crates/loom-store/migrations/`, recorded in `loom_schema_migrations`. This is a
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

Issues are repository-owned. `source_branch` is immutable provenance;
`claimed_branch` is the optional current owner and alone defines a branch's
working set. Removing a session clears its claims back to the backlog rather
than deleting issues. The in-worktree CLI reads its claims plus a bounded
unclaimed backlog; the Loom board reads the repository-wide set.

Visible sessions have one shared Spaces → Groups → Sessions placement.
Attention, All, and History are projections over it. Successful automation is
placed as an ordinary session in Ops; an unmatched provisioning or failed run
is projected as an Intervention in Attention/Ops without inventing a browser-
local session. Hidden warm infrastructure has no placement.
GitHub and Slack triggers have their own origin-default Inbox spaces; delegated
sessions inherit their parent's placement.

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
- **Deployment settings** use runtime rows in `settings`, deployment defaults in
  `deployment_settings`, and immutable defaults declared in
  `weaver-core::config::REGISTRY`; reads resolve in that order. Both binaries
  use the same helpers. Personal overrides use `user_preferences`, keyed by
  username and preference key. See
  [configuration policy](configuration.md). **Per-repo** conventions instead
  live in a committed `.weaver/config.toml` read by
  `weaver-core::repo_config` — distinct from global settings, and resolved
  repo-file → builtin-default like a repo's own `WEAVER.md`.
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

Routes live under `/api`; the Vue SPA, CLI, and MCP adapters are clients of the
same surface. The rule is one line:

> Anything that reaches the API is a registered operation. The only axis that
> varies is response encoding.

### The operation registry

An operation is declared exactly once, in
[`crates/weaver-api/src/operations/`](../crates/weaver-api/src/operations/), and
**its id is its path on disk**: `issues.tags.set` lives in
`operations/issues/tags/set.rs` and nowhere else. A declaration is an
`#[operation(...)]` attribute plus an `Input` struct:

```rust
#[operation(
    id = "sessions.shells.delete",
    actor = SessionSelf,          // which credentials may call this
    scope = Session,              // which resource it is authorized against
    risk = Write,
    grants = ["loom/sessions/write@v1"],
)]
pub struct Delete;

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, Operands)]
pub struct Input {
    /// Which of the session's debug shells to close.
    #[operand(positional)]
    pub index: u32,
    /// A visible session id. Omit for this session.
    #[operand(context)]
    pub session: String,
}
```

Everything else is derived from that:

| projection | derived how |
|---|---|
| REST route | `POST /api/sessions/shells/delete` — the id with dots as slashes. Not declared; computed. |
| JSON Schema | `schemars` over `Input`, minus `#[operand(context)]` fields |
| MCP tool | name, description, and schema from the same declaration |
| CLI command | clap `Command` built from the same `Operands` derive, so an advertised invocation and the parser that accepts it are one value read twice |
| authority | `actor`, `grants`, and the resource named by `Scoped` |

`register::<O>(handler)` in `crates/loom/src/web/` binds the declaration to its
implementation; the type parameters are the check, so a handler cannot accept or
return something the declaration does not promise. Startup asserts the two sets
are equal, so a descriptor nothing serves is a boot failure rather than a 404
found in production.

`#[operand(context)]` marks a field the *dispatcher* supplies — `session`,
`branch`, `repo_root`. It is stripped from the caller-facing schema and filled
server-side from the calling credential, so an agent says "my session" by saying
nothing. An explicit value still wins, and still faces the scope check.

### Response encoding is the only variable

`io` names how an operation answers, and it is a field on the declaration rather
than a reason to leave the registry:

| `io` | method | served by |
|---|---|---|
| `Json` | POST | the generic dispatcher |
| `Session` | POST | hand-written, because the response must carry a `Set-Cookie` |
| `Stream` | GET | hand-written, because the body is an SSE stream |
| `Duplex` | GET | hand-written, because the response is a websocket upgrade |

A `Stream` or `Duplex` operation takes its operands from the query string
(`GET /api/sessions/terminal?session=abc`) rather than from path segments,
precisely so that its real route equals its derived one. Its handler lives in
[`crates/loom/src/web/streams.rs`](../crates/loom/src/web/streams.rs) and calls
the same `authorize` the JSON dispatcher does — a custom encoding does not mean
custom authority.

The multiplexed `events.stream` is a *container*: reaching it grants nothing,
because each requested topic is authorized as the single-topic operation it
stands in for.

### What is not an operation

Three things, and they are enumerated in
[`crates/loom/tests/surface_parity.rs`](../crates/loom/tests/surface_parity.rs)
rather than left to judgement:

* the reverse proxy to the embedded editor, which forwards an arbitrary sub-path;
* unauthenticated infrastructure (`/health`, `/metrics`, `/ready`), the
  HMAC-authenticated GitHub webhook, and the browser OAuth redirects;
* registry discovery (`/api/meta`, `/api/operations`, `/api/openapi.json`) —
  making those operations would be circular.

That file is the ledger, and it is a ratchet. Every hand-mounted route is in one
of three lists: a declared transport (the above), a route an operation already
supersedes (still mounted for a caller that has not moved yet), or a route with
no operation at all. The last list is the registry being incomplete, which is
counted separately because it is a different problem from being duplicated.

Review drafts are REST-private and emit no branch-wide event until submission;
other tabs refresh the creator's draft when they regain focus. `ReviewDto`
separates the subject's internal `id` from its public round-trippable `key`,
returns a monotonic `draft_revision`, and exposes the exact server-rendered
`message`. A 409 optimistic-revision response carries the fresh review under
`details.review`. ACP delivery commits into `review_conversation_inbox`, a
stable-key, branch-addressable immutable lane consumed at turn boundaries.
That lane is distinct from `sessions.pending_prompt`, so the prompt retraction
route can never expose submitted feedback. Delivery workers claim fenced lease
tokens, leave offline targets at zero attempts, and may rehome queued feedback
to the branch's next usable conversation.

`SessionView` (the `sessions.*` operations) returns session-specific fields
top-level (`id`, `status`, `work_dir`, `term_session`, `agent_kind`, `model`,
`effort`, `pending_prompt`, `github_repo`, `github_issue` (the `repo` + `number`
linked on the session's tracking issue), `last_activity_at`,
`created_at`, `updated_at`, `parent_id`, `protocol` (`terminal` or `acp`),
`acp_session_id`, `current_mode`, `usage` (`{used, size}` context window, from
the journal's latest `usage` block), `origin` (the channel that created it:
`user`/`agent`/`github`/`slack`/`watch`/`actions`/`ops`), `class`
(`interactive`/`automation`), `turn_count` (incremented on each `working`
lifecycle edge), `placement` (qualified space/group plus integer rank), and
`tracking_issue` (an optional explicit claimed/imported compatibility work item,
populated on every read)) plus a nested
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
signal is **value-driven**. An attention value (`attention`/`blocked`) raises
the branch on the dashboard whatever its key. The review watch's quiet
`awaiting: review` mark sorts below the calm default because it describes work
waiting on an external actor with nothing for the operator to do. The internal
value sets live in `weaver_core::tags`.

`SessionView::parent_id` is the branch id of the session that **launched** this
one — the parent in loom's session tree — or `null` for a top-level session. It
is stamped onto the `sessions` row at create time from the resolved
`parent_branch` (so reads need no extra query and the link can't drift), and is
`null` too when that parent is later untracked. It is launch provenance for
delegation displays; workbench grouping and ordering come from the session's
canonical space/group placement.

`BranchView::github` is the branch's latest GitHub pull-request snapshot
(`pr_number`, `pr_url`, `pr_state`, `pr_title`, `is_draft`, `review_decision`,
`checks`, `mergeable`, `merged_at`, `head_sha`, `head_updated_at`, `fetched_at`),
or `null` when GitHub polling is off, there is no PR, or the App is unavailable.
See [GitHub integration](#github-integration).

Status is two orthogonal axes. The session's `status` is the **lifecycle**
(orchestrator-owned, mechanical): `created` / `running` / `orphaned` / `done` /
`error` / `archived`. The branch's **`attention` tag** (value
`attention` | `blocked`, absent ⇒ calm) plus its `description` (a one-line
current-state message) are the **agent-declared** "does this need me?" signal,
both set via `loom status`. The dashboard resolves and filters on the
attention signal.

There is **no** `/api/hook` endpoint — see [Status & tags](#status--tags).

**Scratch files** are reference material dropped into the worktree's `scratch/`
directory (git-ignored, so it never enters the agent's diff). They can be added
to a live session via `POST /api/sessions/{id}/scratch`, or attached up-front in
the New Session form: those ride in the create request as `scratch` and are
written *before* the agent launches, with a note appended to the launch prompt
so a fresh agent knows the files are there. The stored branch goal stays the
clean text the user typed. Launch-time and live uploads share a 20-file,
25-MiB-per-file, 50-MiB-total decoded limit and the same filename validation.
The create route has a base64-aware transport envelope, validates the whole
batch before repo/worktree/branch/issue/session side effects, counts ordinary
dotfiles, and reserves `.gitignore` for Loom's `scratch/*` exclusion guard.

**Resolved launch snapshots.** A profile is a reusable template. A canonical
`LaunchSelection` layers optional one-launch selectors over it; omissions
inherit through the template, agent metadata, and policy defaults. Resolve
returns field provenance plus `profile_revision` and a stable
`resolver_revision`. Canonical create, clone, and handoff carry those guards and
receive 409 with a fresh preview on drift. Once accepted, `resolved_launch` is
the concrete non-secret snapshot stamped for that runtime launch. Subsequent
template/default/registry edits cannot silently mutate it; an explicit handoff
can replace it with a newly resolved snapshot.

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
CLI (`loom sessions {send,interrupt,preview}`) and its `nudge` audit uniform across
backends: `send` cancels a live turn and starts the message as a normal prompt,
`interrupt` is a `session/cancel`,
and `preview` renders the last journal blocks as compact plain text instead of
a vt100 screen capture.

**Embedded editor.** The optional editor is a lazy per-session code-server
process rooted at the worktree and bound to loopback with its own authentication
disabled. It is reachable only through Loom's authenticated same-origin
HTTP/WebSocket proxy, so the iframe carries the Loom cookie and cannot choose an
arbitrary upstream or another session's worktree. The proxy preserves
Host/Origin for code-server's WebSocket checks. `sessions.ide_info` is the availability
probe; a development install without code-server remains usable. Archive and
remove stop the editor with the session. The UI exposes it under
Details → Advanced, not as the primary file or work surface.

## Runtime conventions

- **API-first.** New agent features land as executable operation registrations
  first. Routine operations declare arguments and authority beside their typed
  implementation; the registry derives Axum routes and discovery, while CLI and
  MCP call those routes through the REST client. Custom adapters remain for the
  minority of transport-specific shapes. Don't put business logic in
  `bin/loom.rs`, MCP dispatch, or the Vue layer.
- **Errors:** the server returns `AppError` (status + message + optional
  `details` map of per-field reasons); the `loom` CLI uses `anyhow` and prints
  `error: {e:#}`.
- **Async:** tokio everywhere on the server side. External processes (Tapestry
  runtime supervisors, git, gh, and agent adapters) go through
  `tokio::process::Command`. The `loom` CLI remains synchronous-feeling while
  delegating its reads and writes to the internal typed API client over HTTP.
- **Events:** state changes flow through `EventBus`; the SSE handlers in
  `web.rs` fan them out. `loom hook` posts to the branch events route; Loom
  writes the row, and the monitor tick promotes it into session status and a
  fresh `EventBus` notification. Browsers cap HTTP/1.1 at 6 connections per
  origin and an EventSource holds one for its whole life, so the SPA subscribes
  to `events.stream` and receives every topic over that single connection;
  the per-stream routes remain the single-stream API for other clients.
- **No tracking-branch state in the server:** loom can be killed and restarted
  at any time. Terminal *and* relay supervisors and worktrees survive (the
  supervisor is a detached process, independent of `loom server run`); "orphaned"
  is a first-class status, recovered via `loom sessions adopt` (or the Adopt button
  in the UI). On startup and periodically afterward, the active loom generation
  re-attaches every live-relay ACP session missing its in-process driver so its
  journal keeps flowing; a `loom.json` ownership fence prevents an older draining
  server from competing for Tapestry's single relay subscription. ACP cursors
  flush periodically rather than once per streaming frame, and a failed durable
  journal write yields the task so the repair pass replays from the last ACK
  instead of accumulating an unbounded backlog. Tapestry drains durable replay
  in a separate back-pressured task, keeping ACK, write, ping, and replacement
  subscriber control responsive even when the replay exceeds every bounded
  stream buffer. Adopt re-attaches when the relay outlived a crashed task, or
  respawns the adapter and reopens the conversation via `session/load` (falling
  back to a fresh session re-oriented from the goal) when the relay is gone too.
- **No unowned session runtimes:** the database is the ownership authority for
  detached supervisors. Startup and periodic reconciliation remove
  `weaver-<id>` agent supervisors and `loom-shell-<id>-<index>` debug shells
  without a non-archived session/active-launch-reservation owner; inspectable
  `done`/`error` sessions remain owners, and the monitor handles the
  inverse mismatch by marking a session row with no supervisor `orphaned`.
  See [Session lifecycle](session-lifecycle.md).
- **Shell placement and environment follow the agent:** each per-session debug
  shell is derived by the owning Tapestry supervisor. It therefore starts in
  the same Docker container (when configured) with the agent's exact materialized
  launch environment, including profile/repository values and its session-scoped
  credential-broker identity. The global operator Shell is the deliberate
  exception: its supervisor runs beside `loom server run`, giving operators the
  control-plane container's process/filesystem view and its Docker-socket view
  of sibling sessions.

## Status & tags

Two distinct axes (see the SessionView note above): the mechanical **lifecycle**
`sessions.status`, and the agent-declared **attention** carried as a tag on the
branch.

**Tags** are single-valued `(key, value)` annotations on a branch, each with a
`note`, `set_by`, and `set_at`, stored in the shared `tags` table (one row per
`(branch_id, key)`, registry in `weaver_core::tags`).
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
`loom status`, `resolve_attention`, and `loom issues wait` read — so a watch's
outside marks surface on the dashboard without spuriously waking sub-agent
tracking.

**Protocol axis.** Every agent declares an execution `protocol` — `terminal`
(the agent runs in a PTY loom drives by keystroke) or `acp` (a headless adapter
loom drives over the [Agent Client Protocol](https://agentclientprotocol.com)).
The builtins are `terminal`; a custom agent carries its own `custom_agents.protocol`
column. A create may **override** to `acp` where the agent allows it (both Claude
and Codex opt in), and the resolved protocol is stamped on the `sessions` row at
create, immutable thereafter. The row's
protocol — not the agent kind — is what every downstream path (launch, lifecycle,
drive routes, adopt, archive) branches on.

Codex ACP's `agent` mode uses the workspace-write sandbox. Loom sets
`sandbox_workspace_write.network_access` and enables Codex's network proxy with
only `127.0.0.1`, `localhost`, and the ContainerRunner's `loom` network alias
allowed. This lets the injected `$WEAVER_API` remain usable for status,
artifacts, and channels without opening arbitrary shell-command egress. A plain
workspace-write `network_access = true` without the proxy would be broader than
the control-plane requirement.

**Lifecycle** is driven by that protocol. A `terminal` session's lifecycle rides
Claude Code's hooks, so that path merges a `hooks` block into the worktree's
`.claude/settings.local.json` (see `loom::agent::install_hooks` and
`weaver_core::agent::hooks_json`); hookless terminal agents — Codex, and any
custom agent whose `reports_status` is off — start `running` immediately:

| Claude hook event | shells out to |
|---|---|
| `SessionStart` | `loom hook --event session-start` (also injects `additionalContext`: a repository `WEAVER.md` override, or Loom's compact code-registered primer, on a genuine start/resume/clear; after a **compaction** it injects a `loom summary` re-orientation) |
| `UserPromptSubmit` | `loom hook --event working` |
| `Notification` | `loom hook --event waiting` |
| `Stop` | `loom hook --event idle` |

`loom hook` posts an `events` row keyed on the branch resolved from
`$WEAVER_BRANCH` (set by the launcher). Loom's monitor (`apply_hook`)
consumes new `hook` rows on its next tick. A `working` / `waiting` / `idle` hook
means the agent process is alive, so each sets `status = running` (this also
promotes a freshly `launching` session); `session-start` is recorded for the
primer injection but the launch path owns the initial status, so it drives no
liveness here. Liveness is all a work hook proves, so that is all the
orchestrator tracks — it does not infer working/waiting/idle from stillness.

An **`acp` session drives the same lifecycle from the protocol's turn boundaries**
rather than hooks: the acp task calls `monitor::record_acp_lifecycle` at turn
start (`working`) and turn end (`idle`), which records the very `hook` event row
`loom hook` would and then runs the shared `promote_lifecycle` path — so the
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

The **`attention` tag** is otherwise the agent's own call, set via `loom
status set --tag <level> [--message "<message>"]`. That calls `POST /api/branches/{key}/status`,
which writes the tag (and, when a message is given, the `description`) and
records a `tag` event the monitor re-broadcasts over SSE, atomically in one
request — `ok` clears the tag, the two loud levels upsert it. The message rides
the event as its `note`, so the event log carries the full **status trail** —
the progress log the dashboard's activity feed renders, `loom sessions events` prints,
and a GitHub-wired session mirrors publicly (see [GitHub
integration](#github-integration)). Omitting `--message` changes only the level
and keeps the last message. Last write wins, so an explicit declaration
overrides the hook-inferred default. The general `loom sessions tags` group
writes any key the same way, over the
`branches.tags.set` / `branches.tags.delete` operations; the session-scoped
`sessions.tags.set` / `sessions.tags.delete` serve the UI. Watches replace their
complete author-scoped set through one bulk tag transaction. The transaction removes only rows
still attributed to that watch, so a stale round cannot delete a key another
actor took over after the round's fleet snapshot; exact `(key, value)` clears
handle lifecycle marks such as `idle: idle` without a key-only race. The builtin
status watch, when a session goes idle (the agent's finished-turn hook), asks the
judge model for the set of tags the session warrants and reconciles its own
typed marks to that set — never mirroring the agent's own `attention`. When the
judge names a genuine need, that session is actively waiting, not resting, so
the watch *replaces* the soothing `idle` mark with the real loud status; a
"nothing needed" verdict leaves `idle` in place.

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
backs `loom sessions transcript`, which renders the current worktree's (or a named file's)
transcript on demand.

The dashboard surfaces this as a **Conversation tab** on the session detail,
backed by `sessions.conversation` (`chatlog::conversation` → for a
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

Agent recall uses the related
[normalized history/search contract](session-history.md). ACP records come
directly from `chat_blocks`; terminal records reuse the fingerprint-cached Iris
normalizer on read and the archived `chat.json` fallback. The trusted
`loom_session.history` and `loom_session.search` tools call these REST routes;
the older `mcp/history/self@v1` adapter remains compatible. Both resolve the
caller through session-scoped context, so neither adds a parallel data model or
authorization path.

Orphan detection is independent: if the session's supervisor is no longer alive,
the session becomes `orphaned` and is eligible for `loom adopt`.

Archive and recovery serialize their external supervisor/worktree changes.
Archive stops the supervisor before committing `archived`, escalating to runtime
removal and finally committing with a warning if it cannot be confirmed stopped
— archiving is how a person disposes of a session, so it always terminates;
recovery first moves `archived → created` with a compare-and-set, which reserves
the branch's unique active-session slot before it rebuilds or launches anything.
A failed recovery cleans up its new external state before restoring `archived`.
Recovery also repairs historical partial archives by adopting an already-live
supervisor instead of leaving the row in an unusable in-between state.
Both operations publish a durable `lifecycle_transition` (`archiving` or
`adopting`) plus a human-readable `lifecycle_step` while external work is in
flight. REST detail/summary views expose these as `transition`; the SPA shows
the stage and suppresses lifecycle actions until completion. Transition claims
are atomic across overlapping server generations. A marker whose owner process
is gone, or that has outlived `TRANSITION_STALE_SECS`, is abandoned: startup
reconciles it before normal supervisor inventory runs, the monitor reconciles it
again on the retention cadence, and a user-driven archive or recovery takes it
over rather than refusing. A live marker still refuses concurrent operations, so
a draining generation keeps its in-flight teardown.

**Retention and automation lifecycle.** Ordinary interactive sessions inherit a
ten-day idle archive policy (`864000` seconds); a profile override is stamped
onto the session at launch, and an explicit `0` disables that TTL. The monitor
uses durable `last_activity_at`, falls back to `created_at` for untouched work,
and can archive completed/error rows as well as live ones once they are old.

A `class = automation` session — every session not
launched interactively by a human, excluding a watch's own warm sessions —
carries a turn cap (`automation.turn_cap`, default `100`, `0` disables)
counted by `sessions.turn_count`. Exceeding the cap raises a loud `blocked`
attention tag and the ACP driver refuses to start a new turn. The monitor also
reaps automation sessions: a legacy explicit work-item session is archived once
its `tracking_issue_id` closes; any automation session is eligible after
`automation.idle_archive_secs` (default `28800`, `0`
disables) of inactivity — both guarded by a no-live-turn check and a grace
period, so a session mid-turn or only just gone quiet is never torn down out
from under it. Every automatic retention path skips a branch carrying the exact
quiet tag `auto-archive: disabled`; the session Details popover toggles it, while a
manual Archive deliberately ignores it. The `automation.*` settings live in
`weaver-core::config::registry()` under the **Automation** group.

Slack-origin sessions remain interactive but share the retention reaper's
lifecycle safeguards. `slack.idle_archive_secs` defaults to `86400` seconds;
the reaper uses the same durable `last_activity_at`, no-live-ACP-turn guard,
grace period, and per-session `auto-archive: disabled` opt-out before archiving
a Slack conversation.

## GitHub integration

An agent can request access to an additional repository when a live task
expands beyond its launch policy. A human grants or revokes that session-only
access from `loom github access` or Session Details, without restarting it.
Loom validates write access against the GitHub App installation before storing
the audited override.

When the GitHub App is configured, loom keeps a per-branch pull-request
snapshot alongside the session. A second background loop
(`github::poll`, sibling of the monitor, spawned in `server::serve`) runs on
startup and every minute. It polls sessions active in the previous ten minutes
on every tick, the rest of the first quiet day every ten minutes, and older live
sessions hourly (using creation time until first activity). Poll attempts are
remembered in memory so empty results and failures follow the same cadence. For
each due tier it groups sessions by repository and sends one aliased GraphQL
query per repository: one open-head lookup for each live branch, plus exact
lookups for explicitly mapped or previously open PR numbers. The exact lookups
preserve merge/close detection after a PR leaves the open set, without returning
to one GitHub request per session. `<branch>` is the worktree's live HEAD
(falling back to loom's stored branch identity when the worktree cannot be
read), so an agent that switches or renames its branch before opening a PR is
still discovered.
The result — PR
number, URL, state (`OPEN`/`CLOSED`/`MERGED`), draft
flag, `reviewDecision`, a rolled-up `checks` verdict (`passing`/`failing`/
`pending`), head SHA/update age, and mergeability — is written to the loom-owned
`branch_github` table (one row per branch, keyed `branch_id`) and served as
`BranchView.github`.
The dashboard renders it on the session list and session header; `POST
/api/sessions/{id}/github` forces an immediate re-poll.

The loop self-gates and degrades quietly: it is always spawned but does nothing
while the `github.poll` setting is off, the GitHub App is unavailable, or the
repo has no GitHub remote (the repository batch is
logged at debug and skipped). So it is a no-op on non-GitHub repos rather
than a failure.

The session header always renders PR and issue association pills, including an
empty state. Existing associations are direct GitHub links; adjacent edit
controls keep reassociation secondary. The PR editor can pin an explicit number
or return to live-branch discovery through `PUT` / `DELETE
/api/sessions/{id}/github`; the issue editor patches the GitHub link on the
session's weaver tracking issue, which remains the source of truth for that
association.

**Archive on merge.** When a poll finds a branch's PR has merged and
`github.archive_on_merge` is on (the default), loom archives the session
automatically — the same teardown as the Archive button: the terminal killed,
worktree removed, branch and weaver history kept. The worktree is removed with
`--force`, so any uncommitted work in it is discarded; a merged PR is taken to
mean the workstream is done. The session-level `auto-archive: disabled` tag
suppresses this without changing the global policy. Turn the behaviour off
globally with `loom config set github.archive_on_merge false` (or in the
settings pane).
Both settings live in `weaver-core::config::registry()` under the **GitHub**
group.

GitHub snapshot logic lives in `crate::github`: `fetch_pr` (the single-PR App
request used by manual refresh), `fetch_pr_batch` (the repository-wide App
GraphQL request), `apply_refresh_result` / `apply_snapshot` (store → announce →
maybe archive), and `poll` (the loop). The merge-archive decision is split into
`apply_snapshot` so it is testable without contacting GitHub.

**The status card.** A branch carrying the quiet `github` tag
(`owner/name#number` — stamped by the `@loom` trigger, or set by hand with
`loom sessions tags set github …`, format-validated at set time) mirrors its status
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

Authentication is a **server-only** concern — the CLI authenticates like any
other REST client, sending `$LOOM_TOKEN` as a bearer token when set (falling
back to loom's machine-local token). It lets loom be exposed off the loopback
interface (so the dashboard and the API are reachable without an SSH tunnel)
while gating who may drive the fleet. The core (crypto, the tables, the
GitHub OAuth calls) lives in `crate::auth`, deliberately `axum`-free; the HTTP
glue (the middleware, cookie handling, route handlers) lives in `crate::web`.

Every `/api` route except the public health surface (`/api/health`,
`/api/health/live`, `/api/health/ready`, `/api/ready`), the public login surface
(`/api/auth/me`, `/api/auth/login`, `/api/auth/logout`, `/api/auth/github/*`),
the OIDC-authenticated `/api/auth/federate`, and the HMAC-authenticated GitHub
webhook passes through the `require_auth` middleware. The root `/metrics`
aggregate scrape is also public (and intended
to be restricted to the host metrics agent at the deployment edge). The static
SPA needs no API principal. Protected requests resolve the
request to a `Principal` three ways, in order:

- **API token** — an `Authorization: Bearer loom_…` header. This is the token a
  remote `loom` CLI saves with `loom login`, or that an ephemeral client passes
  in `LOOM_TOKEN`. Tokens are random secrets stored only as a SHA-256 hash
  (`api_tokens.token_hash`); the plaintext is shown once at creation. Managed
  under Settings → Account or `loom token`.
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

**Users and roles.** `users` rows are approved human users. A fresh database is
seeded with one owner — whichever GitHub login `LOOM_OWNER_GITHUB` names at
first run. There is no default: leave it unset and no owner row is seeded at
all, so GitHub login has no `users` row to match until it's set (fail closed,
rather than seed a real maintainer login onto an internet-facing deploy).
GitHub login only succeeds for a login that matches a `users` row; an unknown
identity is authenticated by GitHub but rejected by loom. A user may have a
`github_login`, a `password_hash` (argon2), or both. The persisted role is
`admin` or `user`. Both roles operate normal Loom work, use per-session debug
shells, and read diagnostics, watch activity, and redacted logs. Admins
additionally change deployment settings, integrations, profiles, federations,
shared environment, watches, and user access; they can use the host scratch
shell and read the raw operator log. Existing rows migrate to `admin`, while
newly approved users default to `user`. Browser sessions and personal tokens
resolve the current role on every request, so role changes take effect
immediately.

**GitHub OAuth** is configured per-deploy: register an OAuth app and set its id
and secret via Settings → Integrations or the `LOOM_GITHUB_CLIENT_ID` /
`LOOM_GITHUB_CLIENT_SECRET` env vars. The callback is
`<base>/api/auth/github/callback`, where `<base>` is the `auth.base_url` setting
or, unset, `{X-Forwarded-Proto|http}://{Host}`. The login route sets a short
CSRF `state` cookie the callback verifies. Until an app is configured the GitHub
button is hidden and `auth.me` reports `methods.github = false`.

**The machine-local token.** On startup loom mints (and persists, 0600, at
`$WEAVER_HOME/loom-token`) a `kind = 'local'` `api_tokens` row owned by the
primary user, and injects it as `LOOM_TOKEN` into the environments of its own
same-host subprocesses (the agent's terminal, watch scripts) — and the `loom`
CLI reads it. This makes `auth.trust_loopback = false` a fully working mode:
behind a **same-host reverse proxy** (where forwarded requests look like
loopback and so trust must be off) local automation still authenticates via this
token, while remote callers must present their own. The local token is hidden
from the token list and is not revocable from the UI.

**CLI contexts.** The `loom` CLI stores named server URLs in
`$XDG_CONFIG_HOME/loom/config.toml` and their personal API tokens in a separate
mode-0600 `credentials.toml`. A repository `.loom/client.toml` may select a
context by name, but cannot provide a URL or token. Resolution order is
`--context`, an explicit `WEAVER_API`, `LOOM_CONTEXT`, repository selection,
the user default, then local daemon discovery. `LOOM_TOKEN` overrides the
selected context's token unless an explicit context selects a different
endpoint than `WEAVER_API`. The machine-local token fallback is limited to
loopback URLs, so a local token is never sent to a context that names a remote
host.

**Workload federation.** An admin-managed federation mapping fixes the provider,
issuer, exact audience, identity, service tag, and allowed strict automation
profiles. GitHub mappings bind the numeric repository id plus exact workflow
ref, with optional event/ref restrictions. Google mappings bind both the
service account's immutable numeric `sub` and exact `email`; the verified token
must also carry `email_verified = true`. Loom selects a candidate mapping only
to discover its configured JWKS endpoint, then verifies signature, issuer,
audience, algorithm, and all identity claims before minting. Google and the
production GitHub issuer require RS256. The resulting token has only the
`automation` grant and mapped profiles, carries non-secret provider/service-tag
audit context, and expires after ten minutes. A caller obtains a new Google ID
token and exchanges again; no refresh token or service-account key is stored by
Loom. Automation run records and metrics persist the mapping's bounded service
tag, so operators can distinguish Marin, Grafana, and Actions traffic without
using the workload subject as an observability label.

**Restricted sessions.** A restricted profile is a stamped security posture,
not a task template. It is valid only for strict, environment-cleared Claude ACP
automation with `prelude = none`, `mode = default`, no ambient allowlist, and
scoped Claude SDK tool rules. The first prompt is the caller's complete
`session.goal`; Loom does not add `WEAVER.md` or infer rewrite instructions.
Profiles select reviewed built-in MCP capability sets such as
`mcp/github/comment@v1`; the MCP registry expands them into exact permissions at
session creation and derives the trusted adapter command from those stamped
rules. Repository/profile data never supplies executable MCP configuration.
Restricted launch and recovery omit repository environment/setup and Claude
user/project/local settings. Repository reads are path-scoped, and GitHub
mutations use a fixed MCP bridge backed by a session-scoped REST endpoint. Loom
uses the configured GitHub App client to call GitHub's REST API against the
session's fixed repository and linked thread. The restricted agent reaches
GitHub only through this fixed bridge. Allowed rules execute directly;
any remaining ACP permission request is answered with the adapter's one-shot
rejection (or a cancelled outcome), including after `session/load`. Runtime
handoff and permission-mode changes are forbidden. The stock `github_comment`
profile contains the policy only; its reviewed JSON manifest is seeded when
absent and then remains operator-editable.

**Git and GitHub CLI credentials.** The Docker image installs a Git credential
helper and a `gh` wrapper because those clients do not speak Loom's session API.
They are transport adapters, not policy owners: every fresh, resumed, warm, or
handed-off session receives a reserved `LOOM_GITHUB_AUTH_MODE` selected by Loom.
`direct` uses the launching user's write-only PAT stored in Loom Account
settings, `broker` requests the session's short-lived App installation token,
and `disabled` rejects direct GitHub CLI access. The adapters fail closed when
the reserved mode is absent. The `gh` wrapper translates the selected mode into
the stock CLI's native authentication contract for each invocation.
Adapter registration stays in the image's system Git config: linked worktrees
share repository-local config, agents may clone additional repositories, and
the helper must be available before a target repository exists. Session
selection and credentials remain Loom-owned policy; the image registration
only teaches stock `git` and `gh` how to ask for them.

**MCP/profile control plane.** A profile stores `mcp_access` as `none`, `all`,
or an explicit group list. Saving resolves the trusted builtin registry and
enabled, validated custom definitions and pins the exact result to that profile
revision. Launch copies the capability
identities/digests and custom source revisions into
`sessions.policy_mcp_access`, and gives every ACP runtime native `mcpServers`
descriptors whose subprocess tool surfaces are filtered to the stamped rules.
Built-in adapters include `loom_context`, `loom_channel`, `loom_artifact`, and
`loom_session`, alongside the specialized history, messaging, and
fixed-repository GitHub adapters. Resource tools return concise text plus
machine-readable MCP `structuredContent`; their DTOs are the same ones used by
the REST client and CLI. The ordinary first-party adapters obtain their names,
descriptions, input schemas, and invocation target from `OperationSpec`;
genuinely nested or special behavior remains an explicit custom adapter. Builtin
capability digest goldens ensure this declaration migration cannot invalidate a
pinned profile accidentally.
Small remote projections map session defaults when needed and add the
established text presentation. The shared descriptor-driven dispatcher retains
runtime allow-list gating and object-argument checks; authorization and domain
validation remain authoritative at the REST boundary.
Neither an unchanged profile nor recovery re-resolves the current registry.
Custom definitions live under
absolute identities such as `/engineering/search/docs`; their first segment is
the selectable group. A save runs `initialize` and `tools/list`, then optional
tests, through `uv run --script`. Runtime children start from a cleared
environment with only `PATH`, Loom-controlled uv cache/interpreter paths, and
session-scoped Loom API context. Custom code is
admin-authored and dependency-contained, not sandboxed; it cannot use
builtin-reserved group names or enter restricted sessions. Loom also refuses to
remove a server pinned by a profile, or the last server in a group while an
explicit profile selection still references it. See
[MCP and profile control plane](mcp-profiles.md).

**Cookies** are `HttpOnly; SameSite=Lax; Path=/`; the `Secure` attribute is
added when `auth.cookie_secure` is on (set it when loom is reached over HTTPS).
loom terminates no TLS itself — run it behind a TLS-terminating proxy for remote
use. The `auth.*` settings live in `weaver-core::config::registry()` under the
**Authentication** group; the GitHub client id/secret are stored outside the
registry so the secret never rides `settings.get`.

## Watches

A **watch** is a periodic / triggered program over the fleet: it
wakes on a trigger (a cron tick or a session event), surveys the sessions in
scope, and acts within an explicit capability set. The engine (`loom::watch`,
spawned in `server::serve`, self-gated on the
`watch.enabled` setting) runs each **round** under non-optional guardrails
— no-overlap, cooldown, a wall-clock timeout, no-recursion — and records it in
`watch_runs`, the audit trail the panel's round history renders.

A round runs the **program** the watch names:

- **Builtin scripts** — real Python files in `crates/loom-watch/watches/`,
  embedded into the binary and registered in `loom::builtins`:
  `builtin:status` (an opt-in agentic watch that stamps a `triage` mark on a
  stale in-scope session, judging
  via the configured `prompt` through the daemon's one-shot agent when
  available, otherwise leaving its marks untouched),
  `builtin:review-wait` (mark a session whose open, non-draft PR awaits an
  external review — `review_decision` `REVIEW_REQUIRED` — with a quiet
  `awaiting: review` mark that sinks it below the calm default in the fleet
  sort, and clear it once review lands, the PR merges, or it un-drafts; needs
  `mark`), `builtin:pr-label` (add the configured label when a PR first appears;
  enabled by default and needs `mark`) and `builtin:archive-merged` (flag live
  sessions whose PR has merged, excluding those with `auto-archive: disabled`).
  The archive advisor is read-only and opt-in — the actual archive is still
  `github.archive_on_merge`, above. Watches granted the
  `judge` capability are agentic and the engine limits their automatic rounds
  to at most one every 15 minutes; manual runs still bypass the interval.
  Agent judgements use the watch's selected automation-safe ACP profile. The
  stock `watch` profile is Codex `gpt-5.6-sol` at medium effort, in plan mode
  with a cleared environment and no profile-granted external tools or MCP
  servers; a watch's non-empty model/effort fields override the profile
  defaults. The Watch panel and
  `loom watch programs` list the registry; script sources
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
the round's config (`{id, name, program, params, scope, capabilities, profile,
model, effort, dry_run}`), and the script prints one JSON object —
`{outcome, summary, actions}` — as its final stdout line. A non-zero exit, no
result object, or a
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
| `WEAVER_API` | explicit loom URL (server bind input and CLI override) | `http://127.0.0.1:7878` |
| `LOOM_CONTEXT` | named context for the `loom` CLI when `WEAVER_API` is unset | user default |
| `WEAVER_BRANCH` | compatibility branch key set by `loom sessions launch` in the worktree for branch-relative commands | — |
| `LOOM_TOKEN` | explicit bearer token for Loom CLI/MCP and automation; `loom` otherwise uses its selected context credential or a loopback-only machine token | — |
| `LOOM_OWNER_GITHUB` | GitHub login seeded as the owner on a fresh database; unset seeds no owner at all | — |
| `LOOM_GITHUB_CLIENT_ID` / `LOOM_GITHUB_CLIENT_SECRET` | GitHub OAuth app credentials (override the settings-stored values) | — |
| `WEAVER_TAPESTRY_DIR` | directory holding tapestry's per-session control sockets | `$WEAVER_HOME/sock` |
| `WEAVER_TAPESTRY_BIN` | the `tapestry` supervisor binary loom re-execs (else a sibling of `loom`); set by the tests | sibling of `loom` |
| `RUST_LOG` / `EnvFilter` | tracing filter | `loom=info,weaver_core=info,tower_http=warn` |
