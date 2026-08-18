# Loom UI

Loom is a calm operations workbench for supervising coding sessions. The UI is
a thin client of the REST API: organization, drafts, issue mutations, and
lifecycle state are server-backed so the CLI observes the same system.

Live updates arrive over one connection. Browsers cap HTTP/1.1 at six
connections per origin and an EventSource holds one for its whole life, so a
view that opens its own stream spends a slot the rest of the page needs — past
the cap a `fetch()` simply never resolves, with no error and nothing in the
server log. Components subscribe to a named topic through `lib/eventStream.ts`
(`layout`, `logs`, `session:{id}`, `chat:{id}`) and it multiplexes them onto a
single `GET /api/events`. Subscribe and unsubscribe with the component's
keep-alive lifecycle; use `onOpen` to re-snapshot, since a reconnect can leave
a gap.

## App shell and workbench

The desktop rail has stable destinations for **Sessions**, **Channels**,
**Issues**, **Watch**, **Shell**, and **Settings**. Narrow fleet pages use four
bottom destinations plus **More**, which holds Shell and preferences. An open
session makes that bottom edge contextual instead: **Sessions**, **Chat**, and
**Artifacts** are the three durable destinations, followed by **More**. Agent or
Shells, Changes, Details, and Scratch live in the bounded More sheet. The
positions stay stable while the session is open, and every switch preserves its
warm pane.

Sessions is organized as shared **Spaces → Groups → Sessions**. **Later** is a
top-level space ahead of the provenance spaces, keeping deferred work out of
each Inbox while preserving one canonical placement. A session has
one canonical placement and one unqualified task label. A group row shows that
label; cross-group smart views qualify it as `Group / Task`. Attention, All, and
History are views over the same placement rather than separate ownership models.

Successful automation is an ordinary session placed in the Ops space. A failed
or incomplete run without a session appears as a typed **Intervention** in
Attention and Ops, where the operator can inspect or clean it up. There is no
separate Automations destination.
GitHub and Slack triggers land in their respective Inbox spaces. Delegated
sessions inherit their parent's placement.

Search, creator scope, moves, ordering, collapse preferences, and placement
defaults all use the session/layout REST APIs. Creator scope can show everyone,
the signed-in operator, Ops work, their union, or other users. Selecting a row
opens the session without changing its group, and returning to the workbench
restores the current view.

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

On desktop, an ACP session leads with **Conversation**, followed by **Shells**
and **Review**. A terminal-backed session leads with **Agent**, followed by
**Conversation** and **Review**. There is no Overview tab. Phones open a plain
session on Chat and replace the top tab strip with the contextual bottom bar.
Artifacts stays one tap away; Agent, Changes, and ACP Shells remain available
from More without occupying primary navigation.

Conversation is the durable operator/agent exchange. Mid-turn composer feedback
stops the current turn and starts as the next turn, so it cannot sit behind work
the model selected concurrently. Unseen queued text can still be retracted into
the composer for editing without rewriting already-dispatched history, or
promoted with **Stop & send**. Permission requests and runtime controls stay in
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

The session header keeps current state and frequent destinations visible. On a
phone it reduces to the task label, live state, and at most one current status
line; the single Sessions button owns navigation back to the fleet, while
Details and GitHub association controls move into More. On desktop,
a linked GitHub issue or pull request opens directly from its labelled pill;
reassociation is an adjacent secondary action, and an empty pill remains a
discoverable setup action.

**Details** is a popover, not a work tab. It owns task and launch metadata,
status history, lifecycle actions, handoff, and auto-archive policy. The
embedded editor is an optional **Advanced → Open editor** escape hatch. The
Details popover becomes a bounded bottom sheet on phones. Split panels open
beside the work area on wide screens; phone review uses one full-width surface
and keeps the session pane warm behind it instead of squeezing two permanent
panes into the viewport.

Archive stops the runtime and removes the worktree while preserving the branch,
conversation, placement, artifacts, and Loom history. Remove deletes the
session, runtime, worktree, Git branch, and Loom history and returns claimed
issues to the backlog.

## Issues and Scratch

Issues is the server-backed repo board. Selection is stable by issue ID across
filtering and pagination. Close, reopen, tag, untag, and delete apply as one
validated bulk action: if any ID or precondition is invalid, none of the
selected issues changes.

Scratch is inbound reference material, kept out of Git. Launch-time and live
drop targets share the server's file-count, per-file, total-size, and filename
validation. A session shows one attachment inline on a wide tab row; larger
desktop collections use a bounded menu. Phones place the complete Scratch
control in the session's More sheet, where long collections expand within the
bounded sheet instead of competing with the task label or work-surface
navigation. Each drop target is scoped to its active route so a cached session
cannot consume files intended for another session.

## Confirmation and feedback

Consequential fleet or lifecycle actions use the shared focus-managed
confirmation dialog, with the exact target, scope, pending state, and retryable
failure visible in the application. Contextual draft/comment actions use an
inline confirmation when a modal interruption would be disproportionate. The
SPA does not delegate confirmation to native browser prompts.

Status, selection, destructive intent, and failures are expressed with text as
well as color. Controls have visible focus and accessible names. Narrow layouts
retain the same routes and actions; work surfaces may replace one another, but
lifecycle, review, and confirmation semantics do not change.

## Keyboard command model

Loom's operator chrome is keyboard-first. Global navigation uses discoverable
`g …` chords, `?` opens the commands available in the current view, and the
status bar shows a short context-sensitive key legend. Sessions behaves like a
mailbox: `j`/`k` move a stable row cursor, `Enter` opens it, `/` focuses search,
`x` or `Space` changes bulk selection, and `o` toggles disclosure.

Inside a session, `b` or `Escape` returns to the preserved Sessions mailbox.
Editable controls, dialogs, menus, and the live terminal own their keystrokes,
so typing `b` or sending terminal Escape never triggers navigation. The bottom
line advertises `b back to sessions` and `?` remains the complete contextual
help. Number keys `1`/`2`/`3` jump directly to the visible work surfaces
(`Agent`/`Conversation`/`Review` for terminal sessions and
`Conversation`/`Shells`/`Review` for ACP); `[` and `]` move between them.

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

The bottom status line also follows the active session—the mailbox cursor on
Sessions or the route on session detail—and exposes its linked GitHub PR and
issue. PR review, CI, and conflict signals reuse the same semantic colors as the
full GitHub panel and the same fleet snapshot, without another request.

Terminal density and split-pane structure are attached to the existing
components through shared tokens and a small inspector component. Light mode
overrides the same semantic tokens, so it remains an explicit preference rather
than a second implementation. Loading, empty, error, and disabled states keep
the resolved layout stable.
