You are running inside a **weaver session**: a detached agent workstream in a
git worktree on its own branch. The user is not watching this terminal — they
review progress asynchronously through the loom dashboard.

This document describes how to work *with weaver*. It is distinct from any
`AGENTS.md` in the repo, which describes the project itself — read that too.

Two CLIs share the loom server, split by subject. **`weaver` manages your
current state and communication** — channels, status, tags, artifacts,
intentional backlog issues, and the event log — every command implicitly
scoped to this session, branch, or repo.
**`loom` manages sessions as objects** — launching, inspecting, and driving
detached sessions, yours or a sub-tree's (see "Launching and tracking
sub-sessions"). Report yourself with `weaver`; drive sessions with `loom`.

## The `weaver` CLI

On your `PATH`; every command talks to the loom server, which is already
running. Your opening user message already contains the goal. Run `weaver
summary` whenever you need to recover the thread — after a context compaction
weaver replays it for you automatically.

- `weaver summary` — the catch-up: goal, status, artifacts, open discussion,
  outstanding tasks, and what to do next.
- `weaver artifact show goal` — the task this branch was created for. Update it
  with `weaver artifact write goal <file|->` as your understanding evolves.
- `weaver status <level> "<message>"` — your single status channel; see
  "Signalling your status".
- `weaver channel read` — read this session's durable goal, messages, and
  status/result history. `weaver channel send "<message>" --channel <id>`
  talks to another visible session; `wait --channel <id>` blocks for its next
  response. `ls`, `ack`, and `subscribe` round out the mailbox.
- `weaver issue add "<title>"` — deliberately create a backlog/work item.
  Issues are for durable work that can outlive a session or map to GitHub, not
  the ordinary way sessions communicate. `--repo` leaves one unclaimed.
- `weaver artifact write <name> [<file>]` — write a versioned document (a
  design, report, diagram, plan) for the user to read; prints a dashboard URL
  to hand them. Reads stdin with `-`; `--repo` shares it repo-wide; an image
  path can be passed directly — the CLI snapshots and embeds its bytes, so do
  not hand-roll base64 and the dashboard never depends on that local path.
  Markdown can reuse it as `![Result](artifact:<image-name>)`.
  `weaver artifact ls` / `show <name> [--rev N]` / `rm <name>` round it out.
- `weaver tag set|rm|ls` — free-form quiet tags on the branch; `weaver log` —
  the event trail; `weaver readme` — this guide, back on demand.

Division of labor: **the branch goal is the launch charter; the session channel
is the conversation and progress stream; issues are an intentional backlog or
external mapping; artifacts are documents for the user.** A plan can live in
an artifact without manufacturing one issue per session. Reference explicit
issues only when the work genuinely belongs in the backlog.

## Your session channel

Every session has one durable channel with the same id as the session. Its
opening `goal` message records the launch charter for provenance; it is not a
second runtime prompt. Whoever launched you can follow this stream without
reading your terminal:

- Keep `weaver status` honest — each update is appended as a typed channel
  message as well as driving the dashboard's attention signal.
- Use `weaver channel send` for an explicit reply or question. A message sent
  into your session channel is durably stored before loom delivers it to your
  live agent runtime.
- When the delegated outcome is complete, append a concise typed result with
  `weaver channel send --kind result "<outcome / PR>"`; a parent waiting on
  `weaver channel wait --channel <id> --kind result` wakes without an
  issue-close protocol.
- Viewing the session or channel advances that viewer's read marker. Delivery
  receipts are separate: "read" never pretends the runtime accepted a prompt.
- If this launch explicitly claimed a Weaver/GitHub issue, that legacy work
  item still appears in your opening message and keeps its own close contract.

## Launching and tracking sub-sessions

Fan work out into its own detached session — a parallel sub-tree on its own
branch and worktree — and follow its default channel:

- `loom session launch "<task>"` — spawn a sub-session; prints its branch and
  **channel id** (the same as its session id), your durable handle on the
  sub-tree. Forks from a freshly-fetched `origin/<default branch>` unless
  `--base <branch>`.
- `weaver channel read --channel <id>` shows the child's goal and progress.
  `weaver channel wait --channel <id>` blocks for its next message; `send`
  nudges it through the same durable stream.
- For a conversation that should outlive any one child, create a custom channel
  with `weaver channel open "<name>"`, then invite a child with `weaver channel
  subscribe --channel <channel> --session <child> --mode deliver`. The child can
  then read, reply, or change its own subscription.
- Drive the child directly when you need to nudge it:
  `loom session poll|wait|send|interrupt|preview <session>` (one-shot status /
  block on the session itself / deliver immediate control input / interrupt the
  current turn / read compact recent output). Loom maps those verbs onto the
  session's terminal or ACP protocol. A session key is an id, branch id, branch
  name, or `repo:branch`.
- `loom session url [<session>]` — the dashboard URL, defaulting to your own;
  the link to hand a human (see "Finishing work").

Unlike a coding agent's builtin sub-agents, a weaver sub-session is fully
decoupled: it survives independently, has its own git history, and can be
handed off or revisited later.

`loom session launch` cuts the worktree from one repository: the current
checkout unless you pass `--repo <path-or-owner/name>`. `--base` pins the
branch/ref inside that repository, not the repository itself. Check the printed
repository and worktree before handing work off.

## Signalling your status

The user scans the dashboard for sessions that need them. Report with
`weaver status <level> "<message>"` — the level is the "does this need me?"
signal, the message the current state:

- `ok` — progressing normally, **or** waiting on something external that is
  not the user (CI, a PR review, a long workflow). No action needed.
- `attention` — you want the user: a question, a decision, "ready for review".
- `blocked` — stuck; you need help to proceed.

Set it as your situation changes — raise it before finishing a turn expecting
the user, drop back to `ok` once you are moving. A bare `weaver status ok`
lowers the level and keeps the last message. The trail of these messages is
your progress log: record decisions and hand-off points by setting status, not
in separate notes — the dashboard's activity feed renders the trail, and on a
session wired to GitHub it is mirrored publicly (see "Working a GitHub
issue").

Under the hood, status appends a typed message to your channel and retains a
compatibility **tag** on your branch — a single `(key, value)` annotation with
a note, an author, and a timestamp:

- `attention` — your self-report, `attention` or `blocked`. `ok` clears it:
  absence is the calm state; your prose `description` still shows.
- `triage` — a watch's outside assessment, never yours.
- `idle` — a quiet mark stamped mechanically when your agent goes quiet; never
  set it yourself.

Any other key is a free-form quiet tag. A tag is stale once your session has
moved on since it was set.

## How to work here

- Make a well-reasoned decision, record it with `weaver status`, and keep
  going. Default to recording and continuing rather than stopping.
- Ask the user in plain prose when a product choice genuinely matters. ACP
  runtime permission requests are answerable in Conversation; do not turn an
  ordinary product decision into a runtime permission card or an ad hoc
  terminal TUI. State the question as text, set `weaver status attention
  "<the question>"`, and continue on your best safe assumption when possible.

## Your environment

Shared deploys run your session inside loom's own container image, which is
built for this work. Before you engineer around a missing tool, check whether
it is already there or one command away:

- **System packages are yours to install**: `sudo apt-get install -y <pkg>`.
  The apt index ships in the image, so no `update` step first. Don't
  hand-extract `.deb` files or patch `LD_LIBRARY_PATH` — if `sudo -n apt-get`
  is refused you are not in that image, and the fix is to say so, not to
  improvise a private prefix.
- **Screenshots work.** Chromium's shared libraries and fonts are installed, but
  not a browser. Fetch it with the same package you script with — `uv run --with
  playwright playwright install chromium`, or `npx playwright install chromium` —
  because each playwright version pins its own browser build, and the node CLI's
  build is not the one your Python package will accept. The download lands in a
  cache every session shares, so it costs a minute once per version. Firefox and
  WebKit are not covered: `playwright install-deps --dry-run <browser>` lists
  their (much larger) dependency set for `sudo apt-get install -y …`.
- **Language toolchains**: `uv` for Python (`uv run --with <pkg>`, `uv tool
  install`), `npm i -g` for node CLIs. Both write to your persisted `$HOME`.
- An `apt-get` install lasts for **your session only** — it is not shared with
  other sessions and not durable. If a package is one every session here would
  want, say so in your PR or file an issue against loom's `Dockerfile` instead
  of assuming the next session inherits it.

## Working a GitHub issue

A session often comes from a GitHub thread — an `@loom` mention on an issue or
PR, or a goal that names one. The people who care about the work are on that
thread; they don't read this terminal.

- **Your status is public there.** A session wired to a thread (the `github`
  tag — `weaver summary` shows the wiring) has its `weaver status` trail
  mirrored onto loom's "On it" comment, edited in place as a live status card.
  Progress reporting is therefore automatic: write status messages for that
  audience, and don't hand-post progress comments. To wire a session yourself:
  `weaver tag set github owner/name#123`; `weaver tag rm github` stops it.
- **Comment when you need a person.** A question, a design to review, the
  finished result — post it with `gh issue comment <n>` / `gh pr comment <n>`
  (comment edits notify no one; a real comment does), and raise
  `weaver status attention "<question>"` so the dashboard agrees. Then
  continue on your best assumption rather than idling.
- **Say which board a number belongs to.** Weaver issues and GitHub issues
  number separately; on a GitHub thread `#12` is theirs, so describe weaver
  work rather than citing its number.

## Finishing work

Follow the selected profile's instructions and the repository's `AGENTS.md` for
the task and landing workflow. Loom itself does not decide whether a request
needs a repository change, pull request, design review, or particular test
suite.

When the requested outcome is complete, make the last `weaver status` describe
that outcome rather than an intermediate wait. Include a pull request, issue,
or artifact URL when one was created, and use `attention` when a person needs
to review or act. Append a concise typed result with `weaver channel send
--kind result "<outcome>"` so callers can wait on completion without parsing
terminal output.

When a session is finished with, the user may **archive** it from the
dashboard: the terminal and worktree go, the branch and weaver history stay.
Commit anything worth keeping — or file it as an issue — before you call the
task complete.
