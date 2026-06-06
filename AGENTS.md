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
  tmux + agent process, the monitor, and `git worktree` / `tmux` shell-outs.

Diagram and module-by-module map: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build & test

```sh
cargo build              # backend + Vue SPA (build.rs drives npm/rspack)
cargo test --workspace   # backend unit + integration (needs git, tmux)
cd e2e && npm test       # Playwright UI suite against a real loom
```

Run `./scripts/pre-commit.sh` before committing — it is the fmt + clippy gate CI
enforces (wire it up with `git config core.hooksPath .githooks`). Build/test
internals and the Playwright setup live in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Don't disturb the user's live loom

A real `loom serve` is **machine-global**: one default tmux server and one
`~/.weaver/weaver.db`, normally running the user's agents — including the one
running *you*. So unless the user explicitly asks:

- **Don't** start your own `loom serve` / `loom launch`, create or kill
  `weaver-*` tmux sessions on the default socket, or run broad tmux cleanup
  (`tmux kill-server`, `pkill -f weaver`). Each wipes the user's agents at a
  stroke.
- **If a task seems to need a live loom, ask first.**

To exercise loom/tmux behaviour, extend the test suites — they isolate via a
private `WEAVER_TMUX_SOCKET` + a temp `WEAVER_HOME`. If you must run loom by
hand, isolate it the same way:

```sh
WEAVER_TMUX_SOCKET=loom-dev-$$ WEAVER_HOME=$(mktemp -d) loom serve --addr 127.0.0.1:0
```

## Landing changes

- **Open a PR; never push to or merge `main`.** Branch → `./scripts/pre-commit.sh`
  + `cargo test --workspace` pass → `gh pr create`. A weaver worktree is already
  on its own branch; finishing means opening the PR, not integrating it yourself.
  Holds for every change unless the user says otherwise.
- **Write in the project's voice** — no self-attribution in commits or PRs
  ("Generated with…", "Co-Authored-By: <tool>", and the like).
- **Keep the branch synced with `main`** when it falls behind or conflicts.
- **Drive the PR to green, then hand off — local green is not CI green.** Opening
  the PR starts this step; it doesn't end it. CI runs more than the local gate
  (the Playwright `e2e/` suite, CodeQL, a clean-checkout SPA build), so passing
  locally proves nothing about CI. After pushing: block on
  `gh pr checks <n> --watch --fail-fast`, read feedback (`gh pr view <n> --json
  reviews,comments` and `gh api repos/<owner>/<repo>/pulls/<n>/comments`), then
  fix failures and address comments — replying in-thread — until it's green.
  Only **then** raise `weaver set-status attention "ready for review"`; while CI
  is running you are `ok`, not done. When a review call is genuinely unclear, ask
  via `weaver set-status attention "<question>"` rather than guessing.

## Conventions

- **API-first.** A new feature is a `web.rs` REST route first; the SPA and the
  `loom` CLI both consume it. No business logic in `bin/loom.rs` or the Vue layer.
- **The frontend is a thin REST client** ([[ui-built-on-rest-api]]): every call
  goes through `frontend/src/api.ts` (no inline `fetch`), and
  `frontend/src/types.ts` mirrors the serde structs in `web.rs` by hand (no
  codegen — keep them in sync). Don't invent browser-local features the `loom`
  CLI can't observe.
- Errors, async, the event bus, orphan recovery, and the rest of the runtime
  model: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
