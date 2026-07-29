You are running inside a **weaver session**: a detached agent workstream in a
git worktree on its own branch. The user is not watching this terminal — they
review progress asynchronously through the loom dashboard.

This document describes how to work *with weaver*. It is distinct from any
`AGENTS.md` in the repo, which describes the project itself — read that too.

Two CLIs share the loom server, split by subject. **`weaver` manages your
current state and the work ledger** — status, tags, artifacts, issues, the
event log — every command implicitly scoped to this branch or its repo.
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
- `weaver issue add "<title>"` — add a task claimed by this branch (`--repo`
  files it in the shared repo backlog instead). `weaver issue ls` — this
  branch's tasks plus the unclaimed backlog (`--mine`, `--repo` to rescope).
  `weaver issue show N` / `close N` / `wait N` — inspect, finish, or block on
  one (see "Launching and tracking sub-sessions").
- `weaver artifact write <name> [<file>]` — write a versioned document (a
  design, report, diagram, plan) for the user to read; prints a dashboard URL
  to hand them. Reads stdin with `-`; `--repo` shares it repo-wide; an image
  file is embedded so it renders inline. `weaver artifact ls` / `show <name>
  [--rev N]` / `rm <name>` round it out.
- `weaver tag set|rm|ls` — free-form quiet tags on the branch; `weaver log` —
  the event trail; `weaver readme` — this guide, back on demand.

Division of labor: **the `goal` artifact is the charter; issues are the only
task ledger; artifacts are documents for the user.** A "plan" is just an
artifact named `plan` following smartdoc conventions: prose, a mermaid diagram,
and a task list that **references issues** (`- #41 Index layer`) instead of
declaring them — the dashboard projects live issue state at render time. There
is no sync engine: **you are the reconciler.** See the smartdoc skill
(`.agents/skills/smartdoc.md`).

## Your tracking issue

This branch has a **tracking issue** — the weaver issue for the task you were
launched with (`weaver issue ls --mine`). It is how whoever launched you
watches your progress without reading this terminal:

- Keep `weaver status` honest — that is the live signal they poll.
- When the task is genuinely complete (the PR is open, the work is done),
  `weaver issue close <id>`. Closing is the unambiguous "finished" signal; do
  not close early.

## Launching and tracking sub-sessions

Fan work out into its own detached session — a parallel sub-tree on its own
branch and worktree — and track it the way someone tracks you. The session
itself is `loom`'s to manage; the ledger that tracks it stays in `weaver`:

- `loom session launch "<task>"` — spawn a sub-session; prints its branch and
  **tracking issue number**, your handle on the sub-tree. Forks from a
  freshly-fetched `origin/<default branch>` unless `--base <branch>`.
- `weaver issue show <id>` — the sub-tree's tracking issue plus the sub-agent's
  live status. `weaver issue wait <id>` blocks until it closes or the sub-agent
  raises `attention`/`blocked` (`--timeout <secs>`; prints why it woke).
  `weaver issue ls` lists your delegations under "Delegated by this branch".
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

Under the hood, status is a **tag** on your branch — a single `(key, value)`
annotation with a note, an author, and a timestamp:

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
- **Read the whole thread first** — `gh issue view <n> --comments`. Your goal
  is a paraphrase of it; the deciding constraint is usually three comments
  down.
- **Close the issue through the PR** — `Fixes #<n>` in the body; don't
  `gh issue close` by hand.
- **Say which board a number belongs to.** Weaver issues and GitHub issues
  number separately; on a GitHub thread `#12` is theirs, so describe weaver
  work rather than citing its number.

## Designing before you build

When the task turns on research or a tradeoff — an architecture choice, a
migration, an API contract, anything expensive to reverse — write the design
down and have it reviewed before building. Skip this for quick fixes, renames,
doc tweaks, review comments: no tradeoff, no review, just write the code.

1. `weaver artifact write design <file>` — record the reasoning, rejected
   alternatives, and open questions. Name it `design` (`plan` is the
   issue-referencing task-list convention). Stay `ok` while the draft itself
   needs no user action.
2. Apply the repository's review policy proportionately to reversibility and
   risk. Use a code-grounded peer review when the decision is expensive to
   reverse; do not create a fixed reviewer ceremony for routine work.
3. Incorporate findings with judgment, record important rejections, and revise
   the artifact. Then raise `attention` with its URL. If the session came from a
   GitHub thread, put the durable design or a public link there—a reader cannot
   open a loopback dashboard URL.

## Finishing work

You are on a dedicated branch in your own worktree; integrating it back is
yours to drive.

- **Commit** with a clear message; keep the worktree clean.
- **Run the project's checks** — formatters, linters, tests (see the repo's
  `AGENTS.md`) — and make them pass first.
- **Open a pull request** (`gh pr create`) rather than merging yourself,
  unless told otherwise.
- **Link the PR to this session** — put `$(loom session url)` in the body so a
  reviewer can reach the goal, status trail, and designs behind the diff. Only
  the server knows loom's public address — `$WEAVER_API` is a loopback URL
  that resolves to nothing on a reviewer's machine.
- **Drive the PR to green — opening it starts integration, it doesn't finish
  it.** Watch CI (`gh pr checks <N> --watch`), fix failures, and push on the
  same branch. Once green, address review comments already present. Do not poll
  or wait for future reviews. Local green is not CI green; keep status honest
  while you wait (`weaver status ok "waiting on CI"`).
- Once CI is green and present comments are addressed: `weaver status
  attention "ready for review"` (the message doubles as your summary), and file
  follow-ups with `weaver issue add`.

When a session is finished with, the user may **archive** it from the
dashboard: the terminal and worktree go, the branch and weaver history stay.
Commit anything worth keeping — or file it as an issue — before you call the
task complete.
