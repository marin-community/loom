# Loom UI

Loom is a calm operations workbench for supervising coding sessions. The UI is
a thin client of the REST API: organization, drafts, issue mutations, and
lifecycle state are server-backed so the CLI observes the same system.

## App shell and workbench

The rail has five stable destinations: **Sessions**, **Issues**, **Watch**,
**Shell**, and **Settings**. Session Artifacts and Changes are not global
destinations; they live together under that session's Review surface.

Sessions is organized as shared **Spaces → Groups → Sessions**. A session has
one canonical placement and one unqualified task label. A group row shows that
label; cross-group smart views qualify it as `Group / Task`. Attention, All, and
History are views over the same placement rather than separate ownership models.

Successful automation is an ordinary session placed in the Ops space. A failed
or incomplete run without a session appears as a typed **Intervention** in
Attention and Ops, where the operator can inspect or clean it up. There is no
separate Automations destination.

Search, moves, ordering, collapse preferences, and placement defaults all use
the layout REST API. Selecting a row opens the session without changing its
group, and returning to the workbench restores the current view.

The shell polls a compact summary of active sessions. It fetches archived
summaries only when History opens, and fetches full goal, launch, policy, and
runtime context only for the current desktop mailbox cursor, a row disclosure,
or a session page. Cursor changes are debounced and each fetched detail is
reused by row disclosure. Search still matches goal text on the server, so this
transport hierarchy does not weaken discovery.

## Launch

New Session chooses a repository, task, profile, and optional one-launch
overrides. The resolver previews the concrete agent/model/effort/protocol/mode,
policy provenance, capacity, and validation before creation. Profiles are
templates; each runtime launch receives a concrete resolved snapshot. Later
profile or registry edits cannot silently mutate it, while an explicit handoff
can replace it with a newly resolved snapshot.

The composer also accepts bounded Scratch attachments by browse or drag/drop.
Validation and launch errors stay beside the composer without clearing the
task, overrides, or attachments. A successful launch opens the new session.

## Session detail

An ACP session leads with **Conversation**, followed by **Shells** and
**Review**. A terminal-backed session leads with **Agent**, followed by
**Conversation** and **Review**. There is no Overview tab.

Conversation is the durable operator/agent exchange. Mid-turn feedback steers
when the adapter supports it and otherwise queues for the next turn. Unseen
queued text can be retracted into the composer for editing without rewriting
already-dispatched history. Permission requests and runtime controls stay in
the conversation that produced them; elapsed quiet time is evidence, not an
automatic “stuck” verdict.

Review contains **Changes** and **Artifacts**. Both use the same staged review
workflow:

1. Start or recover a private, server-backed draft for the exact subject
   version.
2. Add anchored comments and an optional overall note. Failed saves leave input
   available for retry, and stale mutations reload the authoritative draft.
3. Submit once to freeze the payload and deliver one coherent review into the
   agent conversation.

Draft text that has not yet been saved stays local to its editor. Navigation or
layout changes that would discard it are stopped with a focused save-or-cancel
choice. Submitted review history is immutable apart from comment resolution and
delivery retry.

The session header keeps current state and frequent destinations visible. A
linked GitHub issue or pull request opens directly from its labelled pill;
reassociation is an adjacent secondary action, and an empty pill remains a
discoverable setup action.

**Details** is a popover, not a work tab. It owns task and launch metadata,
status history, lifecycle actions, handoff, auto-archive policy, and Scratch.
The embedded editor is an optional **Advanced → Open editor** escape hatch,
loaded beside the work area only when requested.

Archive stops the runtime and removes the worktree while preserving the branch,
conversation, placement, artifacts, and Weaver history. Remove deletes the
session, runtime, worktree, Git branch, and Weaver history and returns claimed
issues to the backlog.

## Issues and Scratch

Issues is the server-backed repo board. Selection is stable by issue ID across
filtering and pagination. Close, reopen, tag, untag, and delete apply as one
validated bulk action: if any ID or precondition is invalid, none of the
selected issues changes.

Scratch is inbound reference material, kept out of Git. Launch-time and live
drop targets share the server's file-count, per-file, total-size, and filename
validation. Each drop target is scoped to its active route so a cached session
cannot consume files intended for another session.

## Confirmation and feedback

Consequential fleet or lifecycle actions use the shared focus-managed
confirmation dialog, with the exact target, scope, pending state, and retryable
failure visible in the application. Contextual draft/comment actions use an
inline confirmation when a modal interruption would be disproportionate. The
SPA does not delegate confirmation to native browser prompts.

Status, selection, destructive intent, and failures are expressed with text as
well as color. Controls have visible focus and accessible names. Narrow layouts
retain the same routes and actions; panels may stack, but lifecycle, review, and
confirmation semantics do not change.

## Keyboard command model

Loom's operator chrome is keyboard-first. Global navigation uses discoverable
`g …` chords, `?` opens the commands available in the current view, and the
status bar shows a short context-sensitive key legend. Sessions behaves like a
mailbox: `j`/`k` move a stable row cursor, `Enter` opens it, `/` focuses search,
`x` or `Space` changes bulk selection, and `o` toggles disclosure.

The application owns one prioritized command registry rather than view-local
window listeners. Kept-alive routes register commands only while active, and
transient surfaces get first refusal. Character commands never capture typing
from form controls, contenteditable regions, dialogs, menus, or xterm. The row
cursor uses stable session identity and roving focus; it is deliberately
separate from checkbox selection.

## Visual system

The default presentation is a dense dark terminal workbench, not a separate
themed component tree. On desktop, Sessions is a mailbox split: the compact
fleet remains on the left while a sticky inspector follows the keyboard cursor
on the right. Narrow layouts omit that inspector and keep the same rows,
disclosure, and routes.

Operator chrome, command hints, mailbox rows, paths, identifiers, timestamps,
and structured data use monospace; human conversation and documents retain
readable prose faces. Near-black planes, ruled rows, cursor gutters, borders,
and spacing establish hierarchy before cards or shadows do. Phosphor green is
reserved for commands, live focus, and healthy state; amber and red retain
attention and blocked meaning.

Terminal density and split-pane structure are attached to the existing
components through shared tokens and a small inspector component. Light mode
overrides the same semantic tokens, so it remains an explicit preference rather
than a second implementation. Loading, empty, error, and disabled states keep
the resolved layout stable.
