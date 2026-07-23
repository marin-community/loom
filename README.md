# weaver

A lightweight per-branch task tracker, plus an optional orchestrator that runs
coding agents in managed terminals.

weaver ships two binaries:

- **`weaver`** — the **agent-facing CLI**. It is a thin HTTP client of loom's
  REST API (via the `weaver-api` crate) — every command needs a reachable
  `loom server run`. The agent inside a worktree uses it to read and update
  the branch's **goal**, **description**, **notes**, and the repo's
  **issues** (each claimed by a branch or sitting in the shared backlog).
  Without a running loom, `weaver` fails fast with a plain-text error.
- **`loom`** — the **orchestrator**. It runs the REST + SSE server, hosts a
  Vue dashboard, creates worktrees, launches agents into managed terminals,
  and periodically summarizes each branch's diff against its merge base. It
  is the only process that opens the sqlite database directly.

`loom` owns the sqlite database at `~/.weaver/weaver.db`; `weaver` never opens
it — every read and write goes over HTTP to `loom`.

## Getting Started

The fastest way in is to **have your coding agent set weaver up for you**: open
this repo in Claude Code (or your agent of choice) and tell it to *"set weaver up
for me — follow the Getting Started steps in the README."* The steps below are
written for it to run; do them yourself if you'd rather.

1. **Get a Rust toolchain.** If `cargo` isn't already on the PATH, install it
   via [rustup](https://rustup.rs):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Build the tooling.** From the repo root:

   ```sh
   cargo build
   ```

   This produces `target/debug/weaver` and `target/debug/loom`.

3. **Put both binaries on the PATH.** Symlink them into a directory already on
   `$PATH` (e.g. `~/.local/bin`), so they stay current as you rebuild:

   ```sh
   mkdir -p ~/.local/bin
   ln -sf "$PWD/target/debug/weaver" ~/.local/bin/weaver
   ln -sf "$PWD/target/debug/loom"   ~/.local/bin/loom
   ```

   If `~/.local/bin` isn't on your `$PATH`, add it (e.g.
   `export PATH="$HOME/.local/bin:$PATH"` in your shell profile).

Then start the orchestrator and open the dashboard:

```sh
loom server run     # REST + SSE server, terminal launcher, background monitor
loom open      # open the web UI (http://127.0.0.1:7878)
```

`weaver` requires `loom server run` to be reachable — it resolves the server
from `$WEAVER_API` (falling back to the address loom recorded while serving)
and fails with a friendly error if it can't connect. See [Usage](#usage) for the
full command surface, and [AGENTS.md](AGENTS.md) for the build/test loop and how
to work on weaver itself.

## Architecture

```
weaver CLI ──HTTP (REST)──▶ loom server run
                                │
                                ├─ sqlite ─▶ ~/.weaver/weaver.db
                                ├─ axum REST + SSE (127.0.0.1:7878)
                                ├─ terminal supervisors + git worktree wrappers
                                ├─ agent launcher (Claude / Codex / custom agents)
                                ├─ background monitor (status, orphan detection, hook ingest)
                                ├─ background summarizer (headless agent → branch description)
                                └─ Vue SPA at /
```

Agents call `weaver hook` to report status; the loom monitor sees the new
`events` row on its next tick and flips the session's status. `weaver hook`,
like every other subcommand, is just another HTTP call through the
`weaver-api` client.

## Usage

```sh
# Orchestrator (optional)
loom server run                            # run the daemon (REST + UI + terminals + monitor)
loom session launch "Add a /health endpoint"               # new worktree + terminal + agent, seeded with the task
loom session launch "Refactor the parser" --name parser-refactor   # override the branch slug
loom session launch "Big refactor" --model opus --effort high      # pick model tier + reasoning effort
loom session launch "Fix the flaky test" --repo ~/code/other       # a local checkout other than the cwd
loom session launch "How do I run this?" --repo acme/widgets       # a GitHub repo — cloned on first use
loom session poll <session>           # one-shot status (lifecycle + attention)
loom session wait <session>           # block until it finishes or needs you
loom session send <session> "try the curl again"   # type a message + Enter (trigger an agent round)
loom session break <session>          # send Escape — interrupt the current turn
loom session preview <session>        # print the recent terminal screen
loom session url [<session>]          # its dashboard URL (yours by default) — the link to share
loom ps                               # list active sessions
loom session show <branch>                    # session detail
loom attach <branch>                  # attach your terminal to the session (or use the browser terminal)
loom session archive <branch>                 # tear down terminal + worktree, keep branch + history
loom session adopt <branch>                   # recreate the terminal for an orphaned session
loom session rm <branch>                      # remove worktree + terminal + db row
loom open                             # open the web UI

# Agent-facing (requires a reachable `loom server run`, via $WEAVER_API)
weaver artifact show goal             # print the current goal (the `goal` artifact)
weaver artifact write goal -          # update it from stdin
weaver summary                        # goal + outstanding tasks + next-step hints
weaver status attention "ready for review"   # level (ok|attention|blocked) + current-state message
weaver issue add "Backfill old rows" --body "ETA after the schema change"
weaver issue add "Audit the logger" --repo   # unclaimed repo backlog item
weaver issue ls                       # this branch's work + the repo backlog
weaver issue ls --mine                # just this branch's claimed issues
weaver issue ls --repo                # the whole repo, grouped by branch
weaver issue close 7
weaver issue show 7                   # an issue + the live status of the branch working it
weaver issue wait 7                   # block until a sub-session finishes or needs you
weaver status                     # read: goal + status + open-issue count
weaver where                          # debug: print the current branch's repo / branch / branch-id
weaver log --limit 50                 # recent events for the current branch
weaver chatlog                        # render this worktree's agent conversation as markdown
```

`loom session` is the uniform surface for driving a child session. `loom
session launch`'s positional argument is the **task**: it becomes the branch
goal and the agent's opening prompt, and the `weaver/<slug>` branch name is
derived from it (override with `--name`). Launches resolve the `default` profile
unless you pass `--profile`; a non-strict profile can still be overridden with
`--agent`, `--model`, `--effort`, `--protocol`, or `--mode`. The common case is
just `loom session launch "<what to do>"`. A launch with no task and nothing to pick up prints a
usage hint and exits without launching. New branches fork from a
freshly-fetched `origin/<default branch>` — the latest mainline — unless you
pin a parent with `--base` (also a field in the web create form).

Once a session is up, the other verbs interact with it: `loom session poll`
reads its status, `loom session wait` blocks until it finishes or raises
attention, `loom session send` types a message into the agent's terminal (and
submits it to trigger a round), `loom session break` sends Escape to interrupt
the current turn, and `loom session preview` prints the recent terminal screen.
Each takes a session key — an id, branch id, branch name, or `repo:branch`.

`loom session url` prints a session's dashboard URL — the link to hand a person,
resolved against loom's externally-visible address (the `auth.base_url` setting,
else the address you reached it on). With no key it is the session you are
running inside, which is how an agent links a PR back to the work behind it.

Three flags seed the task from existing work instead of a fresh description:
`loom session launch --issue 123` takes the branch's title / goal / description
from a GitHub issue (via the `gh` CLI), `--claim 7` takes them from an existing
weaver issue and moves it out of the repo backlog, and `--branch <name>` resumes
an existing branch. `loom issue ls` prints the repo's board across branches.

Every launch opens a **tracking issue** claimed by the new branch — the task as
a weaver issue — and the launch prints its number. That number is the handle
for following the session: `weaver issue show <n>` reports the issue plus the
live `status` of the branch working it, and `weaver issue wait <n>` blocks
until the issue closes or that branch raises its attention. The launched agent
is told to keep its status current and close the issue when the work is done.
When an agent already inside a weaver session runs `loom session launch`, the
tracking issue is attributed to it (`source_branch`), so its sub-trees show up
under "Delegated by this branch" in `weaver issue ls` — agents can fan work out
into parallel sub-sessions and poll or block on them the same way a human does.

`loom session launch --model <model> --effort <effort>` (both also selectors in
the web create form) pins the session's model selector and reasoning effort.
The selected agent type advertises its available models/efforts and translates
them into its own launch flags (`claude` and `codex` are built in; a custom
agent takes no model/effort selectors). Both are stored per session, so adopting
a recovered session
resumes with the same settings. Omit either to use the configured default, or
the runtime's own default when no configured default is set.

## Status & attention

Status has two independent axes.

The **lifecycle** (`session.status`) is mechanical and orchestrator-owned:
`created`, `launching`, `running`, `orphaned`, `done`, or `error`. `done` and
`error` are terminal; the rest, including `orphaned`, are recoverable. Claude-
backed sessions drive it via Claude Code hooks installed into
`.claude/settings.local.json` by `loom session launch`. Each hook shells out to
`weaver hook --event {working|waiting|idle|session-start}`, writing an `events`
row the monitor consumes on its next tick; any hook means the agent process is
alive → `running`. When Claude blocks asking the user (the `waiting`/Notification
hook), the monitor raises the branch's **attention** to `attention`; the live
prompt itself is read straight from the terminal, one tab away.

The **attention** axis is the agent's own signal of whether it needs you:
`ok` (going fine, or blocked on something external like a CI run or PR review),
`attention` (a question, a decision, "ready for review"), or `blocked` (stuck,
needs help). Agents set it with `weaver status <level> "<message>"`, which
records both the level and a one-line current-state message; a bare
`weaver status <level>` changes the level and keeps the last message. The
dashboard shows both and lets you filter for sessions that need a human. It
replaces the old guessed working/waiting/idle indicator, which was often wrong —
e.g. it read "idle" while the agent was actually waiting on a background
workflow.

## Adoption

A session's terminal supervisor is independent of the loom daemon: restarting
loom leaves it running, though it does not survive a machine reboot (the sqlite
rows and worktrees do). When the monitor finds a session whose terminal has
vanished, it marks it `orphaned` rather than `done`.

An orphaned session can be adopted — its terminal recreated and its
agent resumed (`claude --continue`):

```sh
loom session adopt <branch>                   # or the "Adopt" button in the web UI
```

Set `server.auto_adopt` to have loom adopt every recoverable session
automatically on startup (off by default):

```sh
loom config set server.auto_adopt true
```

## GitHub

With the `gh` CLI installed and authenticated, loom tracks each active session's
pull request. A background loop polls `gh pr view` for the branch every 30s and
surfaces the result on the dashboard: a link straight to the PR, its state
(open / draft / merged / closed), the review decision (approved / changes
requested / review required), and a rolled-up CI verdict (checks passing /
failing / pending). The session's Overview tab has a **Refresh** button to
re-poll on demand.

Every session detail has a **Conversation** tab that renders the agent's chat
with the model — user turns, replies, thinking, and tool calls — live and
(via the archive capture below) still there to review after the terminal is gone.
For ACP sessions, a prompt submitted while the agent is working steers the live
turn when the adapter supports steering; otherwise loom safely queues it as the
next turn. Unseen queued feedback can be pulled back into the composer with its
**Edit** action or ArrowUp from an empty composer. The live status names visible
thinking, writing, or tool activity and reports how long it has been since the
agent produced an observable update; quiet time is not guessed to mean stuck.

Whenever a session is archived — by the Archive button or automatically on merge
— loom first captures that conversation to disk: it finds the agent's transcript
(Claude Code or Codex), normalizes it, and writes a machine-readable `chat.json`
plus a readable `chat.md` under `session.log_dir`
(default `~/.iris/logs/sessions/<branch>/`). `weaver chatlog` renders the same
log for a live session on the command line.

Once a branch's PR merges, loom archives the session automatically — tearing
down its terminal and worktree while keeping the branch and its weaver history, the
same as the Archive button. Turn either behaviour off in **Settings** or from
the CLI:

```sh
loom config set github.archive_on_merge false   # keep the worktree after merge
loom config set github.poll false               # stop polling GitHub entirely
```

Polling is a quiet no-op for repositories without a GitHub remote, or wherever
`gh` is not installed — nothing to configure to opt out there.

### Trigger sessions from issues

Comment **`@loom`** on a GitHub issue or PR and loom launches a session against
that repo, seeded from the issue, and replies with a link to it. GitHub delivers
the comment to `POST /api/github/webhook`, which verifies the delivery's HMAC
signature and authorizes the commenter against the **approved-user allowlist**
(the same people who can sign in to loom — repo write access is not itself a
grant). Set `LOOM_GITHUB_WEBHOOK_SECRET` and point a repo/org webhook at
`{base}/api/github/webhook` (issue-comment events, `application/json`). See
[docs/github-trigger.md](docs/github-trigger.md).

## REST API (brief)

Loom serves a JSON API under `/api`; the Vue SPA is the primary consumer.

- `GET /api/health`, `GET /api/health/live` (process liveness), and
  `GET /api/ready` (database + migration readiness)
- `GET /metrics` (bounded-label OpenMetrics) and `GET /api/diagnostics`
  (admin-only operational inventory used by Settings → Diagnostics)
- `GET POST /api/sessions`, `GET PATCH DELETE /api/sessions/{id}`,
  `POST /api/sessions/{id}/{note,archive,adopt,github}`,
  `GET /api/sessions/{id}/{diff,log,events}`,
  `GET /api/sessions/{id}/terminal` (WebSocket: xterm.js ⇄ the tapestry PTY)
- `GET /api/branches`, `GET PATCH /api/branches/{id}`,
  `GET POST /api/branches/{id}/issues` (issues claimed by the branch),
  `GET PATCH DELETE /api/issues/{id}`
- `GET /api/repos/recent`, `GET /api/repos/branches?cwd=…`,
  `GET POST /api/repos/issues?repo_root=…` (the repo-wide board / backlog)
- `GET POST /api/repos` (the managed repo store / clone allowlist)
- `POST /api/github/webhook` (the inbound GitHub trigger; HMAC-authenticated,
  outside the login middleware — see [docs/github-trigger.md](docs/github-trigger.md))
- `GET PATCH /api/settings`

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the shape of `SessionView`.

## Server address

`loom server run` binds `127.0.0.1:7878` by default. The running daemon records
the address it bound in `~/.weaver/server.json`, so the `loom` CLI finds a local
server with no configuration. Named contexts make switching between local and
remote servers explicit:

```sh
loom context add local --url http://127.0.0.1:7878
loom login production --url https://loom.oa.dev
loom context ls
loom context use production
loom --context local session ls
```

`loom login` validates a personal API token before storing it. The prompt is
hidden; use `--token-stdin` when a password manager supplies the token. Context
endpoints live in `$XDG_CONFIG_HOME/loom/config.toml` (normally
`~/.config/loom/config.toml`) and tokens live separately in
`credentials.toml`, which is mode 0600. A repository may select one of the
user's contexts by committing `.loom/client.toml`:

```toml
context = "production"
```

Repository configuration names a context but cannot provide an endpoint or
credential. Selection order is `--context`, `WEAVER_API`, `LOOM_CONTEXT`, the
repository selector, the user's default context, then the recorded local
server. `LOOM_TOKEN` overrides a saved context credential unless `--context`
selects a different endpoint than `WEAVER_API`; this prevents an injected local
machine token from being sent to a remote server. `loom context current` shows
what the current directory selects.

## Authentication

loom can be exposed off `127.0.0.1` so the dashboard and the API are reachable
without an SSH tunnel — `loom server run --addr 0.0.0.0:7878`, ideally behind a
TLS-terminating reverse proxy. Access is then gated three ways:

- **Local use needs nothing.** Requests from the loopback interface are trusted
  as the machine owner, so the local `loom` CLI, the agent, and watch
  scripts keep working with zero configuration. (Turn this off with
  `auth.trust_loopback false` behind a *same-host* proxy — see below.)
- **GitHub or password login** for the web UI. The login screen offers
  "Continue with GitHub" once an OAuth app is configured, plus username/password.
  A fresh install approves exactly one user — whichever GitHub login you set as
  `LOOM_OWNER_GITHUB` before first run. There is no default; leave it unset and
  no owner is seeded, so GitHub sign-in won't work until it's set. Add more
  users, set your password, and configure GitHub sign-in under **Settings →
  Account**.
- **Personal API tokens** for remote CLIs and other trusted clients. Mint one
  under **Settings → Tokens** or from a locally authenticated CLI:

  ```sh
  loom token add laptop --expires-days 30  # prints the secret once
  ```

  Then sign in once from the remote machine:

  ```sh
  loom login production --url https://loom.example.com
  loom session launch "Investigate the failing test in #123"
  ```

  For an ephemeral environment, the environment-variable form remains
  available:

  ```sh
  export WEAVER_API=https://loom.example.com
  export LOOM_TOKEN=loom_xxxxxxxx
  curl -H "Authorization: Bearer $LOOM_TOKEN" \
       -H 'content-type: application/json' \
       "$WEAVER_API/api/sessions" -d '{"cwd":"/srv/repo","goal":"..."}'
  ```

- **Federated workflow tokens** for GitHub Actions and Google workloads. Loom
  verifies the workload's OIDC identity and returns an automation-scoped token
  with a fixed ten-minute lifetime. The workflow exchanges again for each run;
  it does not store a personal token or choose one of the day-based lifetimes
  shown on the personal-token page. See
  [Restricted sessions](docs/restricted-sessions.md).

To configure **GitHub sign-in**: register an OAuth app on GitHub with the
callback `https://loom.example.com/api/auth/github/callback`, then paste its
client id and secret into Settings → Account (or set `LOOM_GITHUB_CLIENT_ID` /
`LOOM_GITHUB_CLIENT_SECRET`).

Behind a **same-host reverse proxy** the proxy's forwarded requests appear to
come from loopback, so set `auth.trust_loopback false` and `auth.cookie_secure
true`. Local automation keeps working: loom mints a machine-local token (at
`~/.weaver/loom-token`, mode 0600) and hands it to its own subprocesses, so only
genuinely remote callers need to present a token or log in.

## Configuration

General settings live in the `settings` table of the sqlite database, which only
`loom` opens directly. Agent launch policy and agent environment live in named
profiles instead. `weaver config` is read-only — it fetches general settings
over the REST API. Each known setting is declared in a registry (`config.rs`)
with a label, help text, type, and default.

Edit them in the **Settings** pane of the web UI, or write them straight to
sqlite with `loom config set` (no running server needed):

```sh
weaver config ls
loom profile ls
loom profile show default
loom profile show github_comment
loom profile env secret github_comment GH_TOKEN \
  projects/my-project/secrets/loom-github-ci-token/versions/latest
```

Notable settings:

- Profiles select the agent, model, effort, protocol, ACP mode, session class,
  concurrency/turn/idle limits, prelude, and environment posture. `strict`
  profiles reject launch-time overrides; `env_clear` profiles start from a
  minimal baseline plus their explicit ambient allowlist and layered
  profile/repo env.
- Restricted profiles are Claude ACP automation envelopes for caller-supplied
  prompts. They suppress the Weaver prelude and repository setup/config, clear
  Claude setting sources, expose repository-scoped read tools plus fixed
  server-side GitHub tools selected by the built-in `mcp/github/comment`
  capability set, and have Loom reject every unmatched permission request.
  Loom expands profile capability sets into exact permissions when it stamps a
  session and derives adapter processes from its trusted MCP registry; profiles
  never supply executable MCP configuration. The GitHub credential never enters
  the agent process. The stock
  `github_comment` profile is seeded from its reviewed declarative manifest and
  remains operator-editable after the first seed. It is ready for
  GitHub-originated editorial/comment tasks after an operator adds its
  write-only `GH_TOKEN` environment value. See
  [Restricted GitHub sessions](docs/restricted-sessions.md).
- Profile environment values are write-only: API, CLI, and Settings responses
  expose names and update times, never secret values.
- `server.auto_adopt` — adopt every recoverable session on daemon startup.
- `github.poll` — poll GitHub (via `gh`) for each session's PR, review, and
  check status (on by default; a no-op without `gh` or a GitHub remote).
- `github.archive_on_merge` — archive a session automatically once its PR
  merges (on by default).
- `session.log_dir` — where a session's agent conversation log is captured on
  archive (a normalized `chat.json` + a rendered `chat.md` under
  `<dir>/<branch>/`). Blank ⇒ `~/.iris/logs/sessions`; point it at a persistent
  path when the home dir isn't a mounted volume.
- `terminal.theme` — colour palette for the in-browser terminal: `dark` (the
  classic black background, default) or `light`.
- `auth.trust_loopback` — trust loopback requests as the machine owner (on by
  default; turn off behind a same-host proxy). See [Authentication](#authentication).
- `auth.cookie_secure` — mark the login cookie `Secure` (enable when served over
  HTTPS).
- `auth.base_url` — the public URL loom is reached at, used for the GitHub OAuth
  callback (blank ⇒ derived from the request's Host header).

## Developing weaver

To build, test, or hack on weaver itself, see [AGENTS.md](AGENTS.md) — it has
the full loop, the pre-commit gate, and the project conventions. The short of
it: `cargo build` compiles the backend and bundles the Vue dashboard into the
`loom` binary (needs Node + npm; a Node-less checkout still builds and serves a
placeholder page), `cargo test --workspace` runs the backend suites, and `cd e2e
&& npm test` runs the Playwright UI suite.

## Environment

- `WEAVER_HOME` — state directory (default `~/.weaver`)
- `WEAVER_DB` — database path (default `$WEAVER_HOME/weaver.db`)
- `WEAVER_API` — explicit loom URL; overrides automatic named-context selection
- `LOOM_CONTEXT` — named context to use when `--context` and `WEAVER_API` are unset
- `WEAVER_BRANCH` — override the branch resolver (set by `loom session launch`
  in the worktree)
- `LOOM_TOKEN` — explicit bearer token for CLI clients and automation; normally
  overrides saved context credentials (see [Authentication](#authentication))
- `LOOM_OWNER_GITHUB` — GitHub login seeded as the owner on a fresh database.
  No default; leave it unset and no owner is seeded (GitHub sign-in won't work
  until it is set).
- `LOOM_GITHUB_CLIENT_ID` / `LOOM_GITHUB_CLIENT_SECRET` — GitHub OAuth app credentials
- `LOOM_GITHUB_WEBHOOK_SECRET` — shared secret for the inbound GitHub trigger; until set, the webhook rejects every delivery ([docs/github-trigger.md](docs/github-trigger.md))
- `WEAVER_REPOS_DIR` — managed repo store root (default `$WEAVER_HOME/repos`)
