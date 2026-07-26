# Loom UI

Loom is a calm operations workbench for supervising coding sessions. The
interface keeps fleet state, active work, review, and intervention close
together without turning routine monitoring into a wall of controls.

## Principles

- Show the current state before offering an action.
- Keep the primary path obvious and secondary controls quiet.
- Preserve context when moving between fleet, conversation, review, and
  settings.
- Use durable routes and server-backed state rather than browser-only features.
- Reserve modal interruption for choices that need explicit confirmation.

## App shell

The left sidebar holds the stable destinations: Fleet, Issues, Artifacts, and
Settings. The active destination is visually distinct and remains available at
desktop and narrow widths.

Fleet is the working home. Its tree groups sessions by space and group, while
smart views expose useful cross-cutting states such as running, needs attention,
and completed work. Counts describe the visible session set, and selecting a
row opens that session without changing its grouping.

The main pane owns the current route. Detail routes are deep-linkable, browser
history behaves normally, and returning to Fleet restores the operator's place.

## Visual system

The palette is neutral and low-contrast, with saturated color reserved for
status, selection, destructive actions, and focused controls. Typography favors
compact labels and readable working text. Monospace is used for terminal output,
paths, identifiers, and structured data—not general prose.

Spacing and borders establish hierarchy before shadows do. Dense lists remain
scannable, while editors and conversation content receive more breathing room.
Loading, empty, error, and disabled states occupy the same layout as their
resolved content so the interface does not jump.

## Fleet and launch

Spaces and groups are organizational views over sessions. A session may be
unqualified, in which case Fleet shows it in the unqualified group rather than
hiding it. Moving and qualifying sessions use the REST API so the CLI and UI
observe the same result.

The launch composer supports prompt text, workspace, agent profile, and mode.
The resolved profile is shown before launch. A successful launch opens the
session and uses the server-derived title; Fleet shows that same canonical
title when the operator returns.

Keyboard submission is available when the composer is valid. Validation and
launch failures remain next to the composer, and the draft stays available for
correction.

## Session detail

Session detail has three stable surfaces:

- **Conversation** follows the agent exchange and exposes intervention controls.
- **Review** contains Changes and Artifacts for examining and commenting on
  produced work.
- **Details** contains metadata and advanced controls.

Review comments preserve unfinished text across route changes and failed saves.
When a pending comment or edit would be lost by changing the artifact layout,
the UI keeps the current layout, focuses the draft, and asks the operator to
save or cancel first.

Archive and remove are distinct lifecycle actions. Archive stops active work
while preserving the branch, conversation, placement, and Weaver history.
Remove deletes the terminal, worktree, Git branch, conversation, and Weaver
history, while returning claimed issues to the backlog.

## Issues and artifacts

Issues provide server-backed backlog and assignment state. Operators can inspect
an issue, assign it through the supported workflow, and see the resulting
session state without a separate browser-only model.

Artifacts are available globally and within a session's Review surface. The
same artifact route and comment data are used in both places. Layout changes
must not discard unsaved review text.

## Confirmations and feedback

Destructive or consequential actions use the shared in-app confirmation dialog.
Each prompt names the exact target and describes the scope of the action. The
dialog owns its pending and failure states, keeps failures visible for retry,
and returns focus to the invoking control when dismissed.

The dialog traps focus, closes with Escape when idle, and enters on Cancel so an
accidental confirmation is not the default keyboard outcome. Destructive
actions are explicitly labeled in text as well as color.

Inline confirmations remain appropriate for local, reversible edits that do
not need a modal interruption.

## Responsive and accessible behavior

Narrow layouts retain the same route and action model. Navigation may collapse
and panels may stack, but lifecycle controls, review drafts, and confirmation
semantics do not change. Short viewports keep primary content scrollable rather
than moving actions off-screen.

Interactive controls have visible focus, meaningful accessible names, and
keyboard behavior equivalent to pointer behavior. Status is communicated with
text in addition to color. Persistent failures use an alert region; transient
success feedback may use a polite live region.

## Recurring components

Use shared primitives for status badges, empty states, row actions, panel
headers, and confirmation dialogs. Row actions appear through a trailing menu
or compact button instead of making the entire row ambiguous. New UI behavior
should extend these primitives and the REST client rather than introduce a
parallel interaction model.
