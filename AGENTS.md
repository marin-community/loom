# AGENTS.md

How to hack on Loom itself. **Read this whole file before you start** — it's
short on purpose. Depth lives elsewhere, pull it in when you need it:
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) (internals: module map, REST API,
storage, status model, GitHub integration), and [README.md](README.md) (user docs).
The in-workspace primer is registered in code; run `loom help`, `loom summary`,
and `loom permissions show` to discover the executable workflow surface.

## What Loom is

One public command surface over Loom's REST API:

- **`loom`** — the orchestrator: REST + SSE server, Vue SPA, per-session
  detached Tapestry runtime supervisor + agent process, the monitor, and
  `git worktree` shell-outs. The only process that opens the sqlite db
  (`~/.weaver/weaver.db`) directly. Its CLI and MCP adapters are thin REST
  clients of code-registered operations. There is no separate agent CLI.

Diagram and module-by-module map: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build & test

```sh
cargo build              # backend + Vue SPA (build.rs drives npm/rspack)
./scripts/test-representative.sh # ~5 minute local feedback over logic + feature journeys
cargo test --workspace   # exhaustive backend unit + integration suite
cd e2e && npm test       # exhaustive Playwright UI suite against a real loom
cd python/weaver-loom && uv run pytest   # weaver_loom + builtin watch program logic (server-free)
```

Test placement: Python client/watch and binding logic lives in pytest
(`python/weaver-loom/tests/`, `crates/weaver-py/tests/`); Rust and frontend
module logic stays in unit tests; integration and Playwright tests prove
cross-layer wiring and user journeys. Don't duplicate the same contract across
tiers.

Run `./scripts/pre-commit.sh` before committing — it is CI's deterministic Rust
fmt/clippy plus frontend unit/type/format gate. Wire it up as a hook with
`git config core.hooksPath .githooks`. Keep compile and relevant test checks
proportional to the change, but always run the ones that apply.

Separately, follow the canonical [agent lint-review
policy](docs/lint.md#when-to-run): run `scripts/lint-review.py` for substantive
initial implementations and design/risk-changing follow-ups; skip it for small,
low-risk PRs and small review/CI follow-ups after the branch has already had a
review. The linked policy defines the detailed risk criteria. When skipping,
put one concise reason in the PR/testing notes.

The review is kept out of the commit hook so a slow or flaky agent never sits
in the commit path. Build/test internals and the Playwright setup live in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Don't disturb the user's live loom

A real `loom server run` is **machine-global**: one shared `~/.weaver/weaver.db` and a
set of detached Tapestry runtime supervisors (under `~/.weaver/sock`),
normally running the user's agents — including the one running *you*. The
supervisors are detached, so they outlive `loom server run` and a broad kill is what
wipes them. So unless the user explicitly asks:

- **Don't** start your own `loom server run` or `loom sessions launch` against the default
  `~/.weaver`, kill the user's runtime supervisors, or run broad process cleanup
  (`pkill -f tapestry`, `pkill -f weaver`). Each wipes the user's agents at a
  stroke.
- **If a task seems to need a live loom, ask first.**

To exercise loom behaviour, extend the test suites — they isolate via a temp
`WEAVER_HOME`, which scopes both the db and the tapestry sockets. If you must run
loom by hand, isolate it the same way:

```sh
WEAVER_HOME=$(mktemp -d) loom server run --addr 127.0.0.1:0
```

## Landing changes

The full commit → lint-review decision → PR → CI handoff flow is the
**`pull-request` skill**
([.agents/skills/pull-request.md](.agents/skills/pull-request.md)) — invoke it
when you're ready to land. The rules it enforces:

- **Open a PR; never push to or merge `main`.** Branch →
  `./scripts/pre-commit.sh` + relevant tests → the documented lint-review
  decision → `gh pr create`. A weaver worktree is already on its own branch;
  finishing means opening the PR, not integrating it yourself.
- **Write in the project's voice** — no self-attribution in commits or PRs
  ("Generated with…", "Co-Authored-By: <tool>", and the like).
- **Keep the branch synced with `main`** when it falls behind or conflicts.
- **Drive the PR to green, then hand off — local green is not CI green.** CI runs
  more than the local gate (Playwright `e2e/`, CodeQL, a clean-checkout SPA
  build). After pushing, block on `gh pr checks <n> --watch --fail-fast`, fix
  failures until green, and address any comments already present. Only **then**
  raise `loom status set --tag attention --message "ready for review"` and hand off to the
  coordinator/human final reviewer; while CI runs you are `ok`, not done.

## Conventions

- **API-first.** A new feature is a `web.rs` REST route first; the SPA and the
  `loom` CLI both consume it. No business logic in `bin/loom.rs` or the Vue layer.
- **The frontend is a thin REST client** ([[ui-built-on-rest-api]]): every call
  goes through `frontend/src/api.ts` (no inline `fetch`), and
  `frontend/src/types.ts` mirrors the serde structs in `web.rs` by hand (no
  codegen — keep them in sync). Don't invent browser-local features the `loom`
  CLI can't observe.
- **Small SQLite app — don't flag scale.** `~/.weaver/weaver.db` holds ~hundreds
  of rows. Never raise N+1 queries, missing indexes, denormalization, join cost,
  or other scale/perf concerns — they don't apply here. Favor the clean general
  model ([[scale-appropriate-design]]).
- Errors, async, the event bus, orphan recovery, and the rest of the runtime
  model: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
