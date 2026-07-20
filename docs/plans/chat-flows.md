# Agent chat flow catalogue

Loom's ACP conversation is an operator console for an agent that can change the
world, not a stateless chatbot. Chat conventions are useful when they preserve
that distinction: queued text can be edited because the agent has not seen it;
a completed turn cannot be silently rewritten or regenerated because its tool
calls may already have had side effects.

## Current flows

| Flow | Treatment |
| --- | --- |
| Compose and send | Enter sends; Shift+Enter inserts a newline. Failed sends leave the draft intact. |
| Follow up during a turn | The prompt is durably queued. Steering-capable adapters may consume it into the live turn; other adapters wait for the next turn. |
| Revise unseen feedback | The queued block has **Edit**. ArrowUp in an empty composer performs the same action. Loom atomically retracts the server queue before placing its text in the editor. |
| Resolve a draft/queue collision | Pulling a queue into a non-empty draft preserves both, with queued text first. It never overwrites human input. |
| Deliver queued feedback now | **Force now** steers when supported; **Stop & send** cancels a non-steerable turn first; **Send now** starts an idle turn. |
| Stop work without losing feedback | **Stop** cancels the live turn and leaves unseen queued feedback idle until the operator edits or sends it. |
| Understand live work | The tail names streamed thinking, writing, or the current tool. It shows turn age and time since the last observable update; after 15 seconds of silence the shimmer stops and the UI says `no updates for …`. Silence is not labelled `stuck`. |
| Inspect results | Thinking and tool runs fold; failures open loudly; turn endings retain stop reason and context usage. |
| Agent interaction | Permission requests, modes, model/reasoning controls, slash commands, and `@file` resources stay in the same composer. |
| Navigate a long thread | New output follows the foot until the reader scrolls away; the turn rail jumps between user prompts. |

## Missing or deliberately deferred

1. **Individual queue items.** Storage currently coalesces mid-turn sends into
   one next-turn prompt. Editing the combined prompt is honest, but per-message
   delete/reorder requires a structured queue with stable item ids and resource
   metadata. Do that before adding item-level controls.
2. **Sent-prompt history.** After the unseen queue case, ArrowUp could recall
   earlier user prompts as a *copy*. It must not imply that already-dispatched
   history was edited.
3. **Reconnect state.** The transcript reconnects and reconciles automatically,
   but does not tell the operator when it is replaying after a dropped stream.
   Add a quiet `reconnecting` receipt if disconnects prove confusing in use.
4. **Explicit retry.** A retry of an agentic turn can repeat writes, commands, or
   external actions. Any future retry should copy the old prompt into the
   composer and explain that it starts a new turn; never replay automatically.
5. **Cross-device drafts.** Drafts are currently ephemeral component state. In
   this API-first application, persistent draft recovery should use a small
   server-owned resource with ownership and conflict semantics rather than
   becoming an unobservable browser-only feature.
6. **Conversation forks.** Exploring an alternative from an earlier turn needs
   a new session/branch and clear provenance, not mutation of the canonical
   journal.

One-click “regenerate” and automatic “stuck” labels are intentionally absent.
The former can duplicate side effects; the latter cannot distinguish private
reasoning, a slow tool, provider latency, and a dead process from elapsed time
alone. The observable update age gives a human (or a watch) evidence without
inventing certainty.
