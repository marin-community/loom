# AGENTS.md

How to hack on weaver itself. **Read this whole file before you start** — it's
short on purpose. Depth lives elsewhere, pull it in when you need it:
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) (internals: module map, REST API,
storage, status model, GitHub integration), [README.md](README.md) (user docs),
and [crates/weaver-core/WEAVER.md](crates/weaver-core/WEAVER.md) (the prompt the
in-workspace agent sees). Run `weaver readme` for the agent workflow commands.

## What weaver is

Two binaries over one shared sqlite db (`~/.weaver/weaver.db`, WAL):

- **`weaver`** — the daemon-less agent CLI: goal, status, issues, hook events.
  Works with or without loom.
- **`loom`** — the optional orchestrator: REST + SSE server, Vue SPA, per-branch
  terminal supervisor + agent process, the monitor, and `git worktree` shell-outs.

Diagram and module-by-module map: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build & test

```sh
cargo build              # backend + Vue SPA (build.rs drives npm/rspack)
cargo test --workspace   # backend unit + integration (needs git; spawns tapestry PTYs)
cd e2e && npm test       # Playwright UI suite against a real loom
cd python/weaver-loom && uv run pytest   # weaver_loom + builtin overlooker program logic (server-free)
```

Test placement: pure program/module logic lives in pytest
(`python/weaver-loom/tests/`, `crates/weaver-py/tests/`); Rust integration
tests prove wiring against a live server — don't duplicate logic across them.

Run `./scripts/pre-commit.sh` before committing — the fmt + clippy gate CI
enforces; wire it up as a hook with `git config core.hooksPath .githooks`.
Separately, as a step in the commit → PR flow (see the `pull-request` skill), run
the [agent lint review](docs/lint.md) — `scripts/lint-review.py`, a headless
`claude` sub-agent that errors on the slop fmt/clippy can't catch. It's kept out
of the commit hook on purpose, so a slow or flaky agent never sits in the commit
path. Build/test internals and the Playwright setup live in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Don't disturb the user's live loom

A real `loom server run` is **machine-global**: one shared `~/.weaver/weaver.db` and a
set of detached `tapestry` terminal supervisors (under `~/.weaver/sock`),
normally running the user's agents — including the one running *you*. The
supervisors are detached, so they outlive `loom server run` and a broad kill is what
wipes them. So unless the user explicitly asks:

- **Don't** start your own `loom server run` or `loom session launch` against the default
  `~/.weaver`, kill the user's terminal supervisors, or run broad process cleanup
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

The full commit → lint review → PR → monitor flow is the **`pull-request` skill**
([.agents/skills/pull-request.md](.agents/skills/pull-request.md)) — invoke it
when you're ready to land. The rules it enforces:

- **Open a PR; never push to or merge `main`.** Branch → `./scripts/pre-commit.sh`
  + `cargo test --workspace` → `scripts/lint-review.py` → `gh pr create`. A weaver
  worktree is already on its own branch; finishing means opening the PR, not
  integrating it yourself.
- **Write in the project's voice** — no self-attribution in commits or PRs
  ("Generated with…", "Co-Authored-By: <tool>", and the like).
- **Keep the branch synced with `main`** when it falls behind or conflicts.
- **Drive the PR to green, then hand off — local green is not CI green.** CI runs
  more than the local gate (Playwright `e2e/`, CodeQL, a clean-checkout SPA
  build). After pushing, block on `gh pr checks <n> --watch --fail-fast`, answer
  comments in-thread, and fix failures until green. Only **then** raise `weaver
  status attention "ready for review"`; while CI runs you are `ok`, not done.
- **Wait for the bot reviewers — they're slower than CI.** GitHub's Copilot/Codex
  reviewers post inline comments minutes after the PR opens (not instantly, and
  after the checks pass). Don't call a PR done the moment it's pushed: block for
  their review, or check back after ~30 min — `gh pr view <n> --json reviews,comments`
  plus `gh api repos/{owner}/{repo}/pulls/<n>/comments` for the inline threads —
  then address or reply to each before handing off.

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
