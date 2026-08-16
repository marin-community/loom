# weaver

A lightweight session orchestrator and communication surface for coding agents.

weaver ships two binaries:

- **`weaver`** — the **agent-facing CLI**. It is a thin HTTP client of loom's
  REST API (via the `weaver-api` crate) — every command needs a reachable
  `loom server run`. The agent inside a worktree uses it to read and update
  the branch's **goal** and **description**, the session's durable
  **channel**, and intentional repo **backlog items**.
  Without a running loom, `weaver` fails fast with a plain-text error.
- **`loom`** — the **orchestrator**. It runs the REST + SSE server, hosts a
  Vue dashboard, creates worktrees, launches agents under managed runtime
  supervisors, and monitors their lifecycle. It is the only process that opens
  the sqlite database directly.

`loom` owns the sqlite database at `~/.weaver/weaver.db`; `weaver` never opens
it — every read and write goes over HTTP to `loom`.

## Getting Started

The fastest way in is to **have your coding agent set weaver up for you**: open
this repo in Claude Code (or your agent of choice) and tell it to *"set weaver up
for me — follow the Getting Started steps in the README."* The steps below are
written for it to run; do them yourself if you'd rather.

1. **Build the tooling.** From the repo root:

   ```sh
   cargo build
   ```

   This produces `target/debug/weaver` and `target/debug/loom`.

2. **Put both binaries on the PATH.** Symlink them into a directory already on
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
loom server run     # REST + SSE server, runtime manager, background monitor
loom open      # open the web UI (http://127.0.0.1:7878)
```

`weaver` requires `loom server run` to be reachable — it resolves the server
from `$WEAVER_API` (falling back to the address loom recorded while serving)
and fails with a friendly error if it can't connect. See [Usage](#usage) for the
full command surface, and [AGENTS.md](AGENTS.md) for the build/test loop and how
to work on weaver itself.

## Architecture

`weaver` and the Vue SPA are REST clients of `loom`; only `loom` owns sqlite,
worktrees, supervisors, agents, and background services. See
[Architecture](docs/ARCHITECTURE.md) for the module map, flows, storage model,
and route catalogue.

## Usage

The common operator loop is launch, supervise, review, and archive:

```sh
loom session launch "Add a /health endpoint"
loom session poll <session>
loom session wait <session>
loom session send <session> "try the curl again"
loom session interrupt <session>
loom session url [<session>]
loom session archive <session>
```

The task becomes the branch goal and opening prompt; Loom derives the
`weaver/<slug>` branch name and forks from a freshly fetched default branch.
Use `--repo` for another checkout or managed GitHub repository, `--base` to pin
a parent branch, and `--claim` or `--issue` to seed the task from existing work.
Run `loom session launch --help` for the complete launch surface.

Profiles are reusable templates, not live session configuration. Omitted
selectors inherit from the selected template and agent defaults. The resolver
previews concrete selectors, policy provenance, capacity, and validation before
create. The result is a concrete snapshot stamped for that runtime launch.
Later profile, environment, registry, or default edits cannot silently mutate
it; an explicit handoff can replace it with a newly resolved snapshot. New
Session exposes the same profile picker, editable launch fields, save-as-new
composition, and bounded Scratch drop target as the CLI.

A profile may also carry organization-owned opening instructions. Loom appends
them for every origin that selects the profile: user and delegated launches,
Slack and GitHub triggers, watches, and authenticated automation. This keeps
workflow and response conventions in deployment configuration while Loom's
own prompt supplies only Weaver mechanics and transport context.

Loom seeds lightweight instructions for an untouched `default` profile and
editable `slack` and `github` starters from the same runtime posture. Their
reviewed source lives under `crates/loom-policy/profiles/<name>/`; trigger
settings continue to select `default` for compatibility until an operator
explicitly selects an origin profile. Deployment configuration can manage and
select those profiles together.

`poll` reads lifecycle and attention, `wait` blocks until completion or human
attention, `send` delivers immediate control input, and `interrupt` stops the
current turn. For ACP, immediate control input steers a supported live turn and
otherwise cancels and restarts; the conversation composer uses the same
immediate delivery behavior. Session commands accept an id, branch id, branch
name, or `repo:branch`.

The session title is one canonical, unqualified task label; fleet views add its
Group only when they need a qualified `Group / Task` name. Loom records whether
that label was deterministic, model-generated, human-authored, or issue-owned.
Human and issue labels always take precedence: metadata generation can replace
only an unchanged derived label, or an unchanged generated label after an
explicit regenerate request. Renames use compare-and-swap so a stale tab cannot
overwrite a newer label. Generated labels can be disabled per session.

The dashboard presents the fleet as shared **Spaces → Groups → Sessions**.
Attention, All, and History are smart views over that placement; successful
automation is ordinary work in Ops, while failed reservations remain typed
Interventions. Group rows show the unqualified task label and cross-group views
compose `Group / Task`.

A session opens on Conversation (or Agent for a terminal-backed runtime), with
one Review surface for Artifacts and Changes. Details owns launch metadata,
associations, Scratch, lifecycle actions, profile-first handoff, and the
advanced editor escape hatch. Review comments remain private drafts until one
Submit review action delivers coherent feedback. The Backlog pane supports
stable multi-selection and atomic bulk triage. Destructive actions use
focus-managed in-app confirmation with explicit scope and retryable inline
errors; the SPA never delegates that work to browser prompts.

On return after configured inactivity, Loom may prepare a short source-linked
resumption cue from bounded conversation and immutable artifact inputs. Reading
the cue is model-free; explicit ensure/force actions control generation. Cue and
title assistance run with a cleared environment, no tools, and no MCP. Restricted
session content is excluded unless the operator explicitly enables it, and
source fingerprints prevent stale generated text from overwriting newer state.

`loom session url` prints a session's dashboard URL — the link to hand a person,
resolved against loom's externally-visible address (the `auth.base_url` setting,
else the address you reached it on). With no key it is the session you are
running inside, which is how an agent links a PR back to the work behind it.

Channels are the default coordination surface. Every session is created with a
same-id channel whose opening goal records its charter. Messages are append-only,
read markers are per participant, and delivery is recorded per server-owned
binding. A Slack-origin session binds its own channel to that thread, so one
idempotent `result` append is both the canonical outcome and the external reply.
The dashboard's Channels pane is a dense split mailbox; `weaver channel
list|get|read|send|wait|ack` exposes the same REST model to agents (`ls` remains
an alias for `list`).

Issues remain intentional repository backlog or external mappings.
`source_branch` records provenance;
`claimed_branch` records the branch currently working the issue. The agent CLI
defaults to this branch's claims plus the unclaimed backlog, while the Loom board
shows the repository. Batch commands use the same atomic API as the Issues pane:
`loom issue close 7 9`, `loom issue reopen 7 9`,
`loom issue tag --key area --value ui 7 9`,
`loom issue untag --key area 7 9`, and `loom issue delete 7 9`.
If any ID or precondition is invalid, none of the requested issues changes.

An ordinary launch does not manufacture an issue. `loom session launch` prints
the new session/channel id; a parent follows it with `weaver channel read
--channel <id>` or blocks with `weaver channel wait --channel <id>`. Explicit
`--claim` and GitHub-triggered launches retain a compatibility issue association
because that work item already exists outside the session.

Inside a worktree, the compact agent loop is:

```sh
weaver summary
weaver status set --tag ok --message "implementing the API"
weaver channel read
weaver artifact write design design.md
weaver channel send "ready for review"
```

`weaver readme` prints the complete in-workspace workflow. Each command requires
the current `WEAVER_BRANCH` and a reachable Loom server.

## Status & attention

Status has two independent axes.

The **lifecycle** (`session.status`) is mechanical and orchestrator-owned:
`created`, `running`, `orphaned`, `done`, `error`, or `archived`.
ACP turn boundaries and terminal-agent hooks feed one promotion path; supervisor
loss marks a recoverable session orphaned. Runtime permission requests remain in
the ACP Conversation surface rather than becoming guessed lifecycle state.

The **attention** axis is the agent's own signal of whether it needs you:
`ok` (going fine, or blocked on something external like a CI run or PR review),
`attention` (a question, a decision, "ready for review"), or `blocked` (stuck,
needs help). Agents set it with
`weaver status set --tag <level> --message "<message>"`, which records both the
level and a one-line current-state message; omitting `--message` changes the
level and keeps the last message. `weaver status get` reads the current value. The
dashboard shows both and lets you filter for sessions that need a human. It
replaces the old guessed working/waiting/idle indicator, which was often wrong —
e.g. it read "idle" while the agent was actually waiting on a background
workflow.

## Adoption

A session's detached Tapestry runtime supervisor is independent of the loom
daemon: restarting loom leaves it running, though it does not survive a machine
reboot (the sqlite rows and worktrees do). When the monitor finds a session
whose runtime supervisor has vanished, it marks it `orphaned` rather than
`done`.

An orphaned session can be adopted: Loom recreates the stamped runtime and
resumes it through that runtime's recovery contract.

```sh
loom session adopt <branch>                   # or the "Adopt" button in the web UI
```

Set `server.auto_adopt` to have loom adopt every recoverable session
automatically on startup (off by default):

```sh
loom config set server.auto_adopt true
```

## Recovery

An ACP provider can stay connected but become unusable, with one failed turn
followed by every new message ending in `error`. The Conversation view renders
the latest error in red and offers **Recover** beside it. Recovery restarts only
that session's provider runtime and reloads its provider conversation; the
worktree, branch, durable journal, and session URL stay in place.

The same action is available outside the browser:

```sh
loom session recover <branch>
```

For an archived session, this command retains its existing meaning: rebuild the
kept branch's worktree and resume the agent.

## GitHub

With the `gh` CLI installed and authenticated, loom tracks each active session's
pull request. A background loop polls `gh pr view` for the branch every five
minutes, with an immediate refresh available from Session **Details**, and
surfaces the result on the dashboard: a link straight to the PR, its head-update
age, its state (open / draft / merged / closed), the review decision (approved /
changes requested / review required), and a rolled-up CI verdict. Compact rows
render CI as `OK` / `TESTING` / `FAILED` / `PENDING`; Session **Details** keeps
the associations and an immediate **Refresh** button available without occupying
the workbench header.

Every session detail has a **Conversation** tab that renders the agent's chat
with the model — user turns, replies, thinking, and tool calls — live and
(via the archive capture below) still there to review after the terminal is gone.
For ACP sessions, the conversation composer sends immediately: it steers an
adapter that supports live input, or stops an unsteerable turn and starts the
message as the next turn. Feedback queued by another client stays visible and
can be pulled back into the composer with its **Edit** action or ArrowUp from an
empty composer, or sent immediately with **Stop & send**. The live status names
visible thinking, writing, or tool activity and reports how long it has been
since the agent produced an observable update; quiet time is not guessed to mean
stuck. Cross-session `loom session send` uses the same immediate policy: it
steers a supported live turn, or cancels that turn and starts the message as a
new one.

Whenever a session is archived — by the Archive button or automatically on merge
— loom first captures that conversation to disk: it finds the agent's transcript
(Claude Code or Codex), normalizes it, and writes a machine-readable `chat.json`
plus a readable `chat.md` under `session.log_dir`
(default `~/.iris/logs/sessions/<branch>/`). `weaver chatlog` renders the same
log for a live session on the command line.

Agents can page or literally search that same normalized history through
session-scoped REST. ACP reads its durable journal; terminal agents normalize
their native transcript on read and use the archived Iris capture as fallback.
The `loom_session.history|search` tools are thin structured facades over those
routes; `mcp/history/self@v1` remains compatible. See
[Session history and search](docs/session-history.md) for the record, cursor,
source, and authorization contract.

Ordinary sessions are archived after ten days without activity by default, and
a merged PR is archived immediately — both tear down the terminal and worktree
while keeping the branch and its weaver history, the same as the Archive button.
Profiles can override the idle interval (an explicit `0` disables that TTL). To
keep one session until you archive it yourself, choose **Disable auto-archive**
from that session's **Details** popover, or set its quiet opt-out label:

```sh
weaver tag set auto-archive disabled
weaver tag rm auto-archive                    # allow automatic archive again
```

The opt-out also applies to automation cleanup and the other background
retention paths; a manual Archive still works. Turn the global merge behaviour
or GitHub polling off in **Settings** or from the CLI:

```sh
loom config set github.archive_on_merge false   # keep the worktree after merge
loom config set github.poll false               # stop polling GitHub entirely
```

Polling is a quiet no-op for repositories without a GitHub remote, or wherever
`gh` is not installed — nothing to configure to opt out there.

### Trigger sessions from issues

Include **`@loom`** in a GitHub issue body or issue/PR comment when creating it,
or add the mention later by editing the body, and loom launches a session
against that repo, seeded from the issue, then replies with a link to it. GitHub
delivers the request to
`POST /api/github/webhook`, which verifies the delivery's HMAC signature and
authorizes the requester against the **approved-user allowlist** (the same
people who can sign in to loom — repo write access is not itself a grant). Set
`LOOM_GITHUB_WEBHOOK_SECRET` and point a repo/org webhook at
`{base}/api/github/webhook` (issues and issue-comment events,
`application/json`). See [docs/github-trigger.md](docs/github-trigger.md).

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
  users and roles under **Settings → People & security**, set your password
  under **Settings → Account**, and configure GitHub sign-in under **Settings →
  Integrations**.
- **Personal API tokens** for remote CLIs and other trusted clients. Mint one
  under **Settings → Account** or from a locally authenticated CLI:

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
client id and secret into Settings → Integrations (or set `LOOM_GITHUB_CLIENT_ID` /
`LOOM_GITHUB_CLIENT_SECRET`).

Behind a **same-host reverse proxy** the proxy's forwarded requests appear to
come from loopback, so set `auth.trust_loopback false` and `auth.cookie_secure
true`. Local automation keeps working: loom mints a machine-local token (at
`~/.weaver/loom-token`, mode 0600) and hands it to its own subprocesses, so only
genuinely remote callers need to present a token or log in.

## Configuration

General settings and named launch profiles have separate ownership. Settings
control the server and shared services. Profiles control agent selection,
runtime policy, capacity, environment posture, MCP access, and opening
instructions. Edit both in
Settings or use the operator CLI:

```sh
weaver config ls
loom profile ls
loom profile show default
loom profile show ops --effective
loom profile probe ops
loom mcp ls
loom mcp show mcp/github/comment@v1
loom profile add ops --agent codex --mcp github,messaging
loom mcp add /engineering/search/docs --label "Docs search" \
  --file server.py --tests test_mcp.py
```

Strict profiles reject one-launch overrides. Environment-cleared profiles start
from a minimal baseline plus explicit ambient, profile, and repository values.
Environment reads expose names and source metadata, never literal secret values.

MCP selection is provider-neutral: `none`, `all`, or named groups. Saving a
profile pins exact capability identities and revisions; registry edits cannot
silently widen saved profiles or running sessions. Custom MCP programs are
validated administrator code, not a sandbox. See
[MCP and profile control plane](docs/mcp-profiles.md).

Restricted automation profiles additionally suppress repository-controlled
configuration, expose only stamped tools, reject unmatched permission requests,
and keep provider credentials server-side. See
[Restricted sessions](docs/restricted-sessions.md).

The complete setting registry is self-documenting in Settings and
`weaver config ls`. Environment variables and runtime defaults are catalogued in
[Architecture](docs/ARCHITECTURE.md#environment); standalone deployment values
live in [deploy/README.md](deploy/README.md). Registered settings share one
[configuration precedence policy](docs/configuration.md): runtime override →
deployment default → built-in default.

## Developing weaver

See [AGENTS.md](AGENTS.md) for the proportional test loop, pre-commit gate,
repository conventions, and PR workflow.
