You are running inside a **weaver session**: a detached agent workstream in a
git worktree on its own branch. The user is not watching this terminal — they
review progress asynchronously through the loom dashboard.

This document describes how to work *with weaver*. It is distinct from any
`AGENTS.md` in the repo, which describes the project itself — read that too.

## The `weaver` CLI

It is on your `PATH`. From anywhere in the worktree you can run:

- `weaver goal` — print the task this branch was created for.
- `weaver summary` — a quick catch-up on the branch: the goal, your current
  status, the outstanding tasks, and a hint or two for what to do next. Run it
  when you pick up or resume a branch. After a context compaction weaver replays
  this catch-up for you automatically, so you don't lose track of where you are.
- `weaver readme` — print this guide (the full weaver workflow). Re-read it when
  you need the rules back — e.g. after a compaction, when only the concise
  catch-up was replayed.
- `weaver note "<text>"` — append a progress note, or a decision and its
  rationale, to the branch log.
- `weaver set-status <level> "<message>"` — tell the dashboard how you are
  doing. `level` is one of `ok`, `attention`, or `blocked`; the message is a
  short current-state note ("Wired up routes; tests pass"). This is your single
  channel for reporting status — both the level and the message in one call.
- `weaver issue add "<title>"` — add a task claimed by this branch (`--repo`
  files it in the shared repo backlog instead).
- `weaver issue ls` — this branch's tasks plus the unclaimed repo backlog
  (`--mine` for just yours, `--repo` for the whole repo). `weaver issue close N`.
- `weaver issue show N` — an issue plus the live status of the branch working
  it; `weaver issue wait N` blocks until it closes or that branch needs you
  (see "Launching and tracking sub-sessions" below).
- `weaver plan new "<title>"` — for a large, multi-step effort, scaffold a
  structured plan (a markdown file under `docs/plans/`) holding the design, an
  architecture diagram, and a task breakdown with stable ids. `weaver plan show
  <slug>` prints it with each task's live status; `weaver plan sync <slug>
  --apply` reconciles the plan's tasks into weaver issues (and flags, never
  rewrites, work already in flight). Reach for this only when the work spans
  many sessions; a small task stays goal-plus-issues. See
  `docs/structured-projects.md`.
- `weaver set-status` — with no argument, show the goal, status, and open-issue
  count.

These talk directly to the weaver database — no daemon required.

## Your tracking issue

This branch has a **tracking issue** — a weaver issue claimed by your branch
that represents the task you were launched for. `weaver issue ls --mine` shows
it. It is how the agent (or human) that launched you watches your progress
without reading this terminal:

- Keep your status honest with `weaver set-status` as you work — that is the
  live signal whoever launched you polls.
- When the task is genuinely complete (the PR is open, the work is done), run
  `weaver issue close <id>` on the tracking issue. Closing it is the
  unambiguous "this sub-tree is finished" signal; leaving it open means "still
  in flight". Do not close it early.

## Launching and tracking sub-sessions

You can fan work out into its own detached session — a parallel sub-tree on its
own branch and worktree — and track it the same way someone tracks you:

- `loom launch "<what the sub-agent should do>"` — spawn a sub-session. It
  prints the new branch and a **tracking issue number** for the task; that
  issue is your handle on the sub-tree.
- `weaver issue show <id>` — poll the sub-tree: its tracking issue's
  open/closed state plus the sub-agent's live `set-status` (attention +
  current-state message).
- `weaver issue wait <id>` — block until the sub-tree finishes (its tracking
  issue closes) or needs you (the sub-agent raises its attention to
  `attention`/`blocked`). Takes `--timeout <secs>`; prints why it woke.
- `weaver issue ls` lists the sub-tasks you have delegated under
  "Delegated by this branch", each with its sub-agent's current status.

This duplicates some of a coding agent's builtin sub-agents, but a weaver
sub-session is fully decoupled: it survives independently, has its own git
history, and you can hand it off or revisit it later.

## Signalling your status

The user scans the dashboard for sessions that need them. Keep your status
honest with `weaver set-status <level> "<message>"`. The level is the
"does this need me?" signal; the message says what's going on:

- `ok` — progressing normally, **or** blocked on something external that is not
  the user (a CI run, a PR review, a long workflow). No action needed.
  Example: `weaver set-status ok "waiting on PR review feedback"`.
- `attention` — you want the user to look: a question, a decision to confirm, or
  "done — ready for review". Example: `weaver set-status attention "ready for review"`.
- `blocked` — you are stuck or hit an error and need help to proceed.
  Example: `weaver set-status blocked "build broken, can't reproduce locally"`.

Set it as your situation changes — especially raise it to `attention` before you
finish a turn expecting the user, and drop back to `ok` once you are moving
again. A bare `weaver set-status ok` lowers the level and keeps your last
message. This replaces the old guessed working/waiting/idle indicator, which was
often wrong (e.g. it read "idle" while you were really waiting on a workflow).

## How to work here

- Prefer to make a well-reasoned decision, record it with `weaver note`, and
  keep going. Default to recording and continuing rather than stopping.
- You may still ask the user for feedback in plain prose when a choice genuinely
  matters. But **never block on an interactive TUI prompt** — multiple-choice
  menus, plan-approval dialogs, and the like cannot be answered from the
  dashboard. State the question as plain text, record it with `weaver note`,
  set `weaver set-status attention "<the question>"`, and continue with your
  best assumption.

## Finishing work

You are on a dedicated branch in your own worktree. There is no "merge" button —
integrating your work back is a deliberate act, and it is yours to drive. When
the work is ready:

- **Commit** your changes with a clear message and keep the worktree clean.
- **Run the project's checks** — formatters, linters, pre-commit hooks, and the
  test suite — and make them pass before you call the work done. If the repo
  documents specific commands (often in `AGENTS.md`), use those.
- **Open a pull request** rather than merging into the base branch yourself,
  unless the user has told you otherwise. Most teams gate integration on review
  and CI; respect that. Use the project's normal PR workflow (e.g. `gh pr
  create`).
- Record what you did and any follow-ups with `weaver note`, then raise
  `weaver set-status attention "ready for review"` (the message doubles as your
  concise summary) so the user knows to look.

When a session is finished with, the user may **archive** it from the dashboard:
that tears down this tmux session and removes the worktree, but preserves the
branch and the weaver history (goal, notes, status) for future reference. So
make sure anything worth keeping is committed to the branch or recorded as a
note before you consider the task complete.
