# Artifacts and reviews

Artifacts are versioned documents stored by Loom for a user to read: designs,
reports, plans, diagrams, and the session goal. They are not worktree scratch
files and do not need to be committed to survive session archive.

Review is the staged feedback system shared by Artifacts and Changes. A review
draft stays private until one explicit submission freezes and delivers it to the
agent conversation.

## Artifact contract

An artifact belongs to a repository and optionally to a branch. Branch-scoped
and repo-shared artifacts use the same public name; a branch-scoped artifact
shadows a shared artifact of that name for the session. The immutable internal
artifact ID keeps history and reviews attached to the document that was actually
read even if a name is deleted, reused, or shadowed later.

Every write appends a revision. Reads use the latest revision by default and can
select an older one. Concurrent user and agent writes therefore create history
instead of overwriting a revision. Removing an artifact removes all of its
revisions; individual revisions are not pruned.

Markdown is the primary format. The renderer supports GFM and Mermaid and
projects two live reference forms:

- `#41` resolves an issue and renders its current ledger status.
- `artifact:design` links another artifact.

References inside code spans or blocks remain literal. Smartdoc projection is a
read-time join: the document references issues, while the issue ledger owns task
state. A plan is simply an artifact convention; there is no plan parser or sync
engine.

Image input to `weaver artifact write` is wrapped as a bounded data-URI Markdown
document so it renders through the same surface without adding a blob API.

### The goal artifact

The session goal is the branch-scoped artifact named `goal`. It is the editable,
versioned source read by session restart, handoff, summary, and metadata
assistance. `branches.goal` is a synchronized cache for list and search paths,
not a second independently editable owner. Historical rows without a goal
artifact retain the cache as a compatibility fallback.

### CLI and UI

The agent-facing commands are:

```sh
weaver artifact write <name> [<file>]  # stdin with -
weaver artifact ls [--repo]
weaver artifact show <name> [--rev N]
weaver artifact rm <name> [--repo]
```

Session **Review → Artifacts** lists branch and shared documents, renders a
selected revision, and exposes source editing as a secondary mode. Saving an
edit creates a new revision. The panel can dock in the work area or open beside
Conversation/Agent without creating a global Artifacts destination.

Scratch is the inbound complement: Scratch is reference material the user gives
the agent; artifacts are documents the agent gives the user.

## Review contract

A review subject is exact and versioned:

- an artifact subject is the immutable artifact ID plus revision;
- the Changes subject is the session's bounded change-set version.

Listing a session's reviews returns submitted history plus the authenticated
creator's private draft. Creating a draft is recoverable: repeated creation for
the same creator and subject returns the existing draft rather than splitting
feedback across drafts.

### Draft

A draft contains an optional overall note and zero or more pending comments.
Artifact comments carry quote, prefix, suffix, block position, and revision
anchors; Changes comments carry stable old/new line coordinates from the typed
change set. Comments can be edited, deleted, and re-anchored before submission.

Every persisted mutation carries the current `expected_revision` and advances a
monotonic draft revision. A stale mutation returns the authoritative review so
the client can reload it for inspection instead of silently merging competing
edits. Moving a draft to a newer subject version is guarded: old anchors must be
re-anchored or explicitly acknowledged, while an overall-only draft can retarget
directly.

Draft mutations are creator-private and emit no branch-wide event. Other tabs
owned by that creator refresh their draft when they regain focus. Text still in
an open comment editor is browser-local until saved; dock, pop, and route changes
that would lose it are blocked with a save-or-cancel instruction.

### Submit

`Submit review` performs one transaction:

1. Verify the optimistic draft revision and current subject version.
2. Freeze the overall note and comments.
3. Render the exact delivery message on the server.
4. Record one `review_submitted` event.
5. Enqueue one delivery item.

The submitted review is immutable. Any authenticated operator may resolve or
reopen a submitted comment or retry failed delivery, but only the draft creator
may mutate or discard draft content.

ACP delivery enters a protected conversation inbox separate from retractable
operator prompts. Inbox consumption, journal delivery key, and live-turn claim
share one logical boundary so recovery cannot start a second turn for a review
already recorded in the journal. Offline delivery remains queued without
consuming an attempt, and fenced leases prevent a stale worker from regressing
state. Terminal delivery is at-least-once.

If the reviewed artifact is later removed, submitted history remains readable by
stable review ID. Legacy artifact discussion threads are accepted only as
compatibility history; new feedback uses staged reviews.

The REST routes and storage ownership are catalogued in
[Architecture](ARCHITECTURE.md#rest-api). The agent workflow and reference
conventions live in [the smartdoc skill](../.agents/skills/smartdoc.md).
