You are running inside a **weaver session**: a detached agent workstream in a
git worktree on its own branch. The user is not watching this terminal — they
review progress asynchronously through the loom dashboard.

This document describes how to work *with weaver*. It is distinct from any
`AGENTS.md` in the repo, which describes the project itself — read that too.

## The `weaver` CLI

It is on your `PATH`. From anywhere in the worktree you can run:

- `weaver artifact show goal` — print the task this branch was created for. The
  goal lives as an artifact named `goal`; update it with `weaver artifact write
  goal <file|->` as your understanding evolves.
- `weaver summary` — a quick catch-up on the branch: the goal, your current
  status, the outstanding tasks, and a hint or two for what to do next. Run it
  when you pick up or resume a branch. After a context compaction weaver replays
  this catch-up for you automatically, so you don't lose track of where you are.
- `weaver readme` — print this guide (the full weaver workflow). Re-read it when
  you need the rules back — e.g. after a compaction, when only the concise
  catch-up was replayed.
- `weaver status <level> "<message>"` — tell the dashboard how you are
  doing. `level` is one of `ok`, `attention`, or `blocked`; the message is a
  short current-state note ("Wired up routes; tests pass"). This is your single
  channel for reporting status — both the level and the message in one call —
  and the trail of these messages is your progress log: record a decision or
  where you left off by setting the status, not in a separate note.
- `weaver issue add "<title>"` — add a task claimed by this branch (`--repo`
  files it in the shared repo backlog instead).
- `weaver issue ls` — this branch's tasks plus the unclaimed repo backlog
  (`--mine` for just yours, `--repo` for the whole repo). `weaver issue close N`.
- `weaver issue show N` — an issue plus the live status of the branch working
  it; `weaver issue wait N` blocks until it closes or that branch needs you
  (see "Launching and tracking sub-sessions" below).
- `weaver artifact write <name> [<file>]` — write a versioned document to
  weaver (a design, a report, a diagram, a plan) for the user to read. Prints a
  dashboard URL to hand them. Reads stdin with `-`; `--repo` makes it
  repo-shared so a fan-out of child sessions sees one copy. An image file
  (`.png`, `.svg`, …) is embedded as a base64 data-URI markdown doc, so it
  renders inline — just `weaver artifact write shot screenshot.png`. `weaver
  artifact ls` lists this branch's plus the shared ones; `weaver artifact show
  <name> [--rev N]` prints content; `weaver artifact rm <name>` removes it and
  its history (`--repo` targets the shared one).
- Division of labor: **the `goal` artifact = the charter (what to do); issues = the only task
  ledger; artifacts = documents for the user to read.** A "plan" is just *an
  artifact named `plan`* following smartdoc conventions: prose, a mermaid
  diagram, and a task list whose items **reference issues** (`- #41 Index
  layer`) instead of declaring them. The doc never states status — the dashboard
  projects live issue state at render time. There is no sync engine: **you are
  the reconciler.** File the issues with `weaver issue add`, reference them in
  the doc, and keep the two aligned as work moves. See the smartdoc skill
  (`.agents/skills/smartdoc.md`).
- `weaver status` — with no argument, show the goal, status, and open-issue
  count.

These talk directly to the weaver database — no daemon required.

## Your tracking issue

This branch has a **tracking issue** — a weaver issue claimed by your branch
that represents the task you were launched for. `weaver issue ls --mine` shows
it. It is how the agent (or human) that launched you watches your progress
without reading this terminal:

- Keep your status honest with `weaver status` as you work — that is the
  live signal whoever launched you polls.
- When the task is genuinely complete (the PR is open, the work is done), run
  `weaver issue close <id>` on the tracking issue. Closing it is the
  unambiguous "this sub-tree is finished" signal; leaving it open means "still
  in flight". Do not close it early.

## Launching and tracking sub-sessions

You can fan work out into its own detached session — a parallel sub-tree on its
own branch and worktree — and track it the same way someone tracks you:

- `loom session launch "<what the sub-agent should do>"` — spawn a sub-session.
  It prints the new branch and a **tracking issue number** for the task; that
  issue is your handle on the sub-tree. The new branch forks from a
  freshly-fetched `origin/<default branch>` unless you pass `--base <branch>`.
- `weaver issue show <id>` — poll the sub-tree: its tracking issue's
  open/closed state plus the sub-agent's live `weaver status` (attention +
  current-state message).
- `weaver issue wait <id>` — block until the sub-tree finishes (its tracking
  issue closes) or needs you (the sub-agent raises its attention to
  `attention`/`blocked`). Takes `--timeout <secs>`; prints why it woke.
- `weaver issue ls` lists the sub-tasks you have delegated under
  "Delegated by this branch", each with its sub-agent's current status.

The tracking issue is the high-level handle; `loom session` also drives a child
session's terminal directly, so you can nudge it without attaching:

- `loom session poll <session>` — one-shot status (lifecycle + attention).
- `loom session wait <session>` — block on the session itself (not its issue)
  until it finishes, is lost, or raises attention. `--timeout <secs>`.
- `loom session send <session> "<message>"` — type a message into the
  sub-agent's pane and submit it, triggering an agent round (e.g. to answer a
  question it asked or redirect it).
- `loom session break <session>` — send Escape to interrupt its current turn.
- `loom session preview <session>` — print its recent terminal screen, to see what
  it's doing right now. A session key is an id, branch id, branch name, or
  `repo:branch`.
- `loom session url [<session>]` — the dashboard URL for a session, defaulting to
  your own. The link to hand a human (see "Finishing work").

This duplicates some of a coding agent's builtin sub-agents, but a weaver
sub-session is fully decoupled: it survives independently, has its own git
history, and you can hand it off or revisit it later.

## Launching sessions in the right workspace

`loom session launch` cuts the new worktree from the mainline of *one* repo,
defaulting to whatever checkout you run it in. Say which repo explicitly: pass
`--repo <path>` pointing at (any directory inside) the target checkout, or `cd`
into it first (`--base` only pins the branch, not the repo). The repos live
under `/home/power/code/<repo>/` — e.g. `marin`, `tunix`, `marin-experiments`.
So for marin work:

    loom session launch --repo /home/power/code/marin "<task>"

The branch is always namespaced `weaver/<slug>` regardless of repo, so the
branch name won't tell you where it landed. After launching, check the printed
`dir:` line sits under the intended repo's `.worktrees/` — if you wanted marin
but it reads `/home/power/code/weaver/.worktrees/...`, it went to the wrong repo.

`iris` and `grug` are subsystems inside the marin monorepo, not separate repos —
launch their work into the marin checkout.

## Signalling your status

The user scans the dashboard for sessions that need them. Keep your status
honest with `weaver status <level> "<message>"`. The level is the
"does this need me?" signal; the message says what's going on:

- `ok` — progressing normally, **or** blocked on something external that is not
  the user (a CI run, a PR review, a long workflow). No action needed.
  Example: `weaver status ok "waiting on PR review feedback"`.
- `attention` — you want the user to look: a question, a decision to confirm, or
  "done — ready for review". Example: `weaver status attention "ready for review"`.
- `blocked` — you are stuck or hit an error and need help to proceed.
  Example: `weaver status blocked "build broken, can't reproduce locally"`.

Set it as your situation changes — especially raise it to `attention` before you
finish a turn expecting the user, and drop back to `ok` once you are moving
again. A bare `weaver status ok` lowers the level and keeps your last
message. This replaces the old guessed working/waiting/idle indicator, which was
often wrong (e.g. it read "idle" while you were really waiting on a workflow).

Under the hood, status is stored as **tags** on your branch. A tag is a single
`(key, value)` annotation with a note, an author, and a timestamp. Three keys are
well known:

- `attention` — your self-report, the value being `attention` or `blocked`. This
  is what `weaver status` writes; `ok` is the absence of the tag, so
  `weaver status ok` clears it. Absence is the calm state — a calm branch carries no
  attention tag, only its `description` message.
- `triage` — a watch's outside assessment of your branch, the same
  `attention`/`blocked` ladder but authored by a watch (or `manual`), never
  by you. It is independent of your `attention` tag and carries its own reason
  and attribution.
- `idle` — a soothing, *quiet* mark stamped mechanically when your agent goes
  quiet (a finished turn or a lull): the calm "resting, no one needed" state. It
  is **not** loud — an idle agent does not read as needing the user — and you
  never set it yourself. The status watch may replace it with a real `attention`
  status once it judges the session genuinely needs a human.

Your prose `description` is separate from the tags and is shown even when you are
calm. Any other key is a free-form, quiet tag. A tag is stale once your session
has moved on since it was set (its timestamp predates your last activity).

## How to work here

- Prefer to make a well-reasoned decision, record it with `weaver status`,
  and keep going. Default to recording and continuing rather than stopping.
- You may still ask the user for feedback in plain prose when a choice genuinely
  matters. But **never block on an interactive TUI prompt** — multiple-choice
  menus, plan-approval dialogs, and the like cannot be answered from the
  dashboard. State the question as plain text and
  set `weaver status attention "<the question>"`, then continue with your
  best assumption.

## Working a GitHub issue

A session often comes from a GitHub thread — someone `@loom`-mentioned an issue
or a PR, or your goal names one. That thread is where the people who care about
the work are: they don't read this terminal, and may not read the dashboard.
Anything they need to know goes there.

- **Read the whole thread first** — `gh issue view <n> --comments` (`gh pr view
  <n> --comments`). Your goal is a paraphrase of it; the constraint that decides
  the design is usually three comments down.
- **Reply where you were asked** — a question for the issue's author is a
  `gh issue comment <n>`. Post it, raise `weaver status attention "<question>"`
  so the dashboard shows what you're waiting on, then continue on your best
  assumption rather than idling.
- **Close the issue through the PR** — write `Fixes #<n>` in the body and let the
  merge close it. Don't `gh issue close` by hand.
- **Say which board a number belongs to.** Weaver issues and GitHub issues number
  separately and will collide. On a GitHub thread `#12` is theirs — a weaver
  issue number tells a reader there nothing, so describe the work instead.

## Designing before you build

When the task turns on research or a tradeoff — an architecture choice, a
migration strategy, an API contract, a storage model, anything expensive to
reverse — write the design down and have it reviewed before you build it. Skip
this for a quick fix, a bug with an obvious cause, a rename, a doc tweak, a
review comment, an issue reply: no tradeoff, no review, just write the code.

1. Write it as an artifact — `weaver artifact write design <file>`: the
   reasoning, the alternatives you rejected and why, the open questions. Name it
   `design`; `plan` means the issue-referencing task list above. Stay `ok` — a
   draft awaiting review is not something the user needs to look at.
2. Send it to two reviewers, each checking it against the code. If you are
   `claude`:

   ```sh
   ask="Peer-review the design on stdin against this repo. What is wrong, missing,
   or over-built? Be concrete and blunt — no praise, no summary."
   weaver artifact show design | codex exec -s read-only "$ask"
   weaver artifact show design | claude -p --model fable "$ask"
   ```

   If you are `codex`: one of your own sub-agents, plus `claude -p --model fable`.
   Run both in the background and in parallel — each takes minutes. If a reviewer
   isn't on `PATH`, note it and go with one.
3. Incorporate the findings and rev the artifact. The reviews will be partly
   wrong; use judgment, not a patch queue. Fix what lands, record what you
   rejected and why. `weaver artifact write design` appends the revision. Only
   now surface it: raise `attention` with the URL, and if the session came from a
   GitHub issue or PR, paste the design on that thread (`gh issue comment <n>` /
   `gh pr comment <n>`) — a reader there can't open a loopback dashboard URL.

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
- **Link the PR back to this session** — `loom session url` prints the dashboard
  URL for the session you are in. Put it in the PR body, so a reviewer looking at
  the diff can reach the context behind it: the goal, your status trail, the
  designs and reports you wrote. Build the body in one go:

      loom session url                        # https://loom.example.com/s/ab12cd34
      gh pr create --title "…" --body "$(printf 'What this does…\n\nloom: %s\n' "$(loom session url)")"

  Only the server knows loom's public address, which is why this is a command and
  not something you assemble yourself — the `$WEAVER_API` you were handed is a
  loopback address that resolves to nothing on the reviewer's machine.
- **Drive the PR to green — opening it is the start of integration, not the
  end.** Do not walk away the moment it's open. Watch CI to completion
  (`gh pr checks <N> --watch`) and fetch review feedback — both the top-level
  reviews (`gh pr view <N> --json reviews,comments`) and inline code comments
  (`gh api repos/{owner}/{repo}/pulls/<N>/comments`). Local green is not CI
  green: fix any failing check and address every review comment with follow-up
  commits on the same branch, then re-watch. Keep `weaver status` honest while
  you wait (`weaver status ok "waiting on CI"`).
- Once CI is green and review is addressed, raise `weaver status attention
  "ready for review"` (the message doubles as your concise summary, and file any
  follow-ups as issues with `weaver issue add`) so the user knows to look.

When a session is finished with, the user may **archive** it from the dashboard:
that tears down this terminal session and removes the worktree, but preserves the
branch and the weaver history (goal, status, events) for future reference. So
make sure anything worth keeping is committed to the branch or filed as an issue
before you consider the task complete.
