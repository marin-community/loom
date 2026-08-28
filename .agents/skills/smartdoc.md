---
name: smartdoc
description: Write and maintain artifacts — versioned documents you hand the user to read (designs, reports, plans). Covers the reference conventions, the plan-as-artifact pattern, and your duty to keep the doc and the issue ledger aligned. Use when drafting a design, writing a report, or running a plan a fan-out shares.
---

# Skill: Smartdoc

An artifact is a document you write *to Loom*, not the repo: a design, a
report, a diagram, a plan. It is versioned, survives archive, and is rendered by
Loom for the user to read. `loom artifacts write <name> [<file>]` (stdin with
`-`) prints a dashboard URL — hand it to the user in your status or a PR comment.
An image file (`.png`, `.jpg`, `.svg`, …) is auto-embedded as a base64 data-URI
markdown doc so it renders inline — pass the image straight to `write`, no
hand-rolled data URI. `loom artifacts delete <name>` removes one and its history.

## When to write one

- A design loop, a report, a plan — anything the user should **read** rather
  than **run**. Out of the repo by default: no checked-in scratch doc nobody
  wants to merge.
- It is one command, not a ceremony. A small task stays goal-plus-issues; reach
  for an artifact only when there's a document worth reading.
- A committed design doc is still a normal repo file on a normal PR. Loom just
  isn't its manager.
- A design that turns on research or a tradeoff goes out for peer review before
  it is built.

## The division of labor

- **goal** — the charter: what to do. The `goal` artifact: `loom artifacts
  write goal <file|->`.
- **issues** — the only task ledger: state lives here, nowhere else.
- **artifacts** — documents for the user to read.

Don't put the same fact in two of them. When the goal is really a document, set
the goal to one line plus a reference and write the artifact.

## Reference conventions

Author markdown; the render projects references against live state.

- `#41` — references a Loom issue; renders as a live status chip
  (open / claimed / closed) read from the ledger, never from the text.
- `artifact:<name>` — links another artifact.
- Mermaid — author diagrams directly; the renderer draws them.
- A reference inside a code span or block is **not** projected.

## The plan-as-artifact pattern

A plan is an artifact named `plan`: prose, a mermaid architecture diagram, and a
task list whose items reference issues you filed.

```markdown
## Tasks
- #41 Index layer — storage + read path
- #42 Executor — depends on #41
- [ ] decide single-node vs distributed (open question, not yet an issue)
```

File each task with `loom issues add` (`--repo` for the shared backlog), then
reference it. The doc references; the issue ledger owns state; the render joins
them and stamps each `#41` live. The doc never says "done".

## The maintenance duty

**You are the sync engine.** There is no reconcile verb — the judgment is yours:

- The plan changes → update the issues. Close the unstarted; *talk about* the
  in-flight rather than yanking it. Use judgment, not a literal diff.
- An issue changes shape → update the doc's references to match.
- A fan-out shares the plan → republish `--repo` so child sessions see the
  update.

When the user changes the plan mid-flight, they tell you; you adjust the ledger.

## Versioning

Every write is an immutable revision; `show` reads latest by default
(`--rev N` for an older one). The user can edit in the dashboard — that creates
a new revision, not a lost update. Concurrent agent and user edits are safe:
last write is a new revision.
