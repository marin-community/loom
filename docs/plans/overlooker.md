---
plan: overlooker
status: draft
---

# The Overlooker — periodic, triggered watch agents

A weaver [plan](../structured-projects.md) (issue
[#61](https://github.com/rjpower/weaver/issues/61)): the design surface and the
task breakdown for one large effort. This file owns the *structure* — the
problem, the architecture, the tasks and their dependencies; the issue ledger
owns the *state* (which task is open / in-flight / done). Nothing here ships
yet — it is the design of record to build against. The [Tasks](#tasks) section
materialises into weaver issues on `weaver plan sync overlooker`.

## Problem & goal

You launch a handful of sessions over a morning, wander off, and come back to a
dashboard of unknowns. Which of these are quietly stuck retrying the same test?
Which finished an hour ago and are sitting idle waiting for a nudge nobody gave?
Which raised `attention` while you were in a meeting? Today the only answer is
*you*, scanning the fleet by hand.

weaver already has the parts for one session to watch another:
`weaver issue wait`, `loom session {poll,wait,send,break,preview}`. A **parent
session** can monitor its children — block on them, read their screens, nudge
them. But that only works if you *set it up in advance*: you have to launch the
parent first and hand it the children. The common reality is the opposite:

- You didn't create an overseer up front, and now you have five **unrelated**
  sessions — different repos, launched at different times, no common parent —
  that would all benefit from the same periodic once-over.
- The judgement you want is *recurring and cross-cutting* ("every hour, look at
  everything that isn't idle and tell me what needs me"), not a one-shot
  parent→child relationship wired at launch.

So the missing primitive is a **retroactive, scheduled, fleet-wide watcher**: a
thing that wakes on a trigger — a clock tick, or a session event — surveys
whatever sessions exist *right now*, applies judgement, and acts: marks a
session as fine, tags one that looks stuck, nudges a third, escalates a fourth
to you. It is not tied to any one workstream. It is infrastructure that watches
the workstreams.

## What the field does (and what to steal)

Five reference points, each contributing one idea.

| Pattern | What it is | The idea to steal | Where it hurts |
|---|---|---|---|
| **Supervisor agent** (LangGraph, Databricks, Strands) | a central LLM coordinator that routes tasks to specialist sub-agents, monitors, retries, synthesises | a single agent that *holds the fleet in its head* and exercises judgement over it | the supervisor is a single point of failure and a cost/latency bottleneck; if its plan hallucinates, everything downstream is wasted |
| **Scheduled / heartbeat agents** (cron-triggered, "paperclip" heartbeat) | an agent woken on an interval; each wake it gets a *context package* (recent activity, outstanding tasks, new inputs) and runs a checklist | wake → compose a fresh snapshot → reason → act → sleep; the snapshot, not the agent's stale memory, is the source of truth for the round | a dumb cron "runs a job"; the value is only there if the woken thing *reasons* — and unbounded reasoning on a timer burns money |
| **Kubernetes reconciler** (controller pattern) | a loop that observes current state, diffs against desired, acts to close the gap — **level-triggered**, not edge-triggered | treat an event as a *nudge to re-survey the whole world*, not as a thing to react to once; idempotent, self-healing, no missed-event replay | desired-state reconciliation assumes a declarative target; "is this agent stuck?" has no clean `spec` to diff against — the judgement is fuzzy |
| **Rules engine** (trigger → condition → action) | declarative automations: an event fires, a condition filters, an action runs; priorities, stop-conditions | the clean **trigger / scope / action** decomposition, and that *most* checks are cheap mechanical rules that need no LLM at all | pure rules can't make the judgement call ("does this screen *look* stuck?") that motivated the whole thing |
| **Stuck-agent watchers** (Wink, ClawKeeper, HITL guardrails) | a watcher monitors an agent's tool-use / loops and intervenes — nudge, interrupt, or escalate to a human past a threshold | the **intervention ladder** (observe → nudge → interrupt → escalate) and that autonomy must be *bounded* and *auditable* | a watcher that acts too eagerly is worse than none — it interrupts healthy work and erodes trust |

The synthesis writes itself: a **level-triggered reconciler** (K8s) whose
"reconcile" step is a **bounded supervisor agent** (LangGraph) woken by a
**cron-or-event trigger** with a **fresh context package** (heartbeat), most of
whose decisions are cheap **rules** (rules engine) and whose actions climb a
**bounded, audited intervention ladder** (stuck-agent watchers).

## The name

A weaving shed full of power looms was tended by an **overlooker** (the
Lancashire term; "tackler" in some mills): the person who walked the rows, kept
the looms running, spotted the one throwing a fault, and fixed or flagged it.
Not a weaver — a *watcher of* weavers and their looms. It is the exact word for
"the thing that looks over the looms," and loom is already our session
orchestrator. So:

- An **overlooker** is one configured watch agent.
- A **round** is one execution of it (it "walks the shed" / "does its rounds").
- A **mark** is the assessment it stamps on a session — the new status indicator
  the problem statement asks for.

"Overseer" stays as the plain-language synonym in prose; **Overlooker** is the
noun in the code and the UI.

## TL;DR

1. **One new subsystem, three nouns.** An **overlooker** (a watch definition), a
   **trigger** (when it wakes), and a **round** (one execution). Managed in a
   dedicated **Overlooker panel**, separate from the session fleet — they are
   infrastructure, not workstreams.
2. **Reuse the session machinery verbatim.** An overlooker's round runs through
   the same `agent::launch` + tmux + hooks + `loom session
   {preview,send,break}` primitives that already exist. We are adding a
   *scheduler and a panel*, not a second agent runtime.
3. **Level-triggered, not edge-triggered.** A trigger (cron tick or session
   event) is a *nudge to re-survey the whole scoped fleet*, never a one-shot
   reaction. The round observes current state and reconciles. This makes it
   idempotent, crash-safe, and immune to missed events — the K8s insight applied
   to agents.
4. **A third status axis: the mark.** Lifecycle (`session.status`) and attention
   (`branch.attention`, agent self-reported) stay untouched. The overlooker
   writes a *separate* `triage` axis — *its* assessment of a session — so it
   never stomps what the agent said about itself. The dashboard shows both
   badges.
5. **Two authoring surfaces, one engine.** A **declarative** overlooker (trigger
   + scope + prompt, no code) covers the headline case. A **scripted**
   overlooker — a Python file against a PyO3-bound `weaver` module — is the
   power-user escape hatch for arbitrary trigger logic and mechanical rules. Both
   run on the same trigger engine.
6. **Bounded and audited by construction.** Least-privilege capabilities
   (observe/mark/escalate by default; nudge/interrupt/launch opt-in), a
   per-round time + token budget, a cooldown, a no-recursion rule, and every
   action recorded as an event. Autonomy you can trust because you can see it and
   cap it.

The rest argues each point.

## The core model: overlooker, trigger, round

An **overlooker** is a stored definition — a row, eventually editable in the
panel — with five fields:

- **Trigger** — *when* it wakes. One of:
  - `cron` — a crontab expression (`0 * * * *`, every hour) or a fixed interval.
  - `event` — a filter over the `events` stream (e.g. *any branch raises
    attention to `blocked`*; *a session has had no activity for 30 min*; *a PR's
    checks went red*).
  - `manual` — only the "Run now" button / CLI.
- **Scope** — *which* sessions a round considers. A query over the fleet:
  `all`, or filtered by attention (`attention != ok`), lifecycle (`status =
  running`), repo, staleness, label. The example overlooker's scope is "every
  session that isn't idle."
- **Mode** — *how* the round executes (see [Execution](#execution-reuse-the-session-machinery)):
  `warm` (a long-lived nudged session), `headless` (a one-shot `claude -p`), or
  `script` (a Python file — which itself decides whether to call an LLM).
- **Instructions** — *what* it does: the prompt (declarative/warm/headless) or
  the script path (scripted).
- **Capabilities** — *what it is allowed to do* (see [Capabilities &
  safety](#capabilities--safety)).

A **round** is the unit of work: trigger fires → engine composes a context
package over the scope → the agent reasons and acts → the engine records the
outcome. Rounds are the audit trail; the panel lists them.

### The mark: a third status axis

The problem statement asks for the overlooker to "mark them ok, or tag a new
status indicator for the session." That indicator must be a **distinct axis**,
not a write to the existing two:

- `session.status` is the **lifecycle** — mechanical, orchestrator-owned.
- `branch.attention` is what the **agent says about itself** — "I'm blocked",
  "ready for review". The overlooker must never overwrite this; conflating "the
  agent declared blocked" with "the overlooker thinks it's stuck" destroys the
  signal that made watching worthwhile.
- `branch.triage` (new) is **the overlooker's assessment**: `ok` /
  `attention` / `blocked`, plus a one-line `triage_note`, plus `triage_by` (which
  overlooker) and `triage_at` (when). It mirrors the `attention` mechanism
  exactly — a denormalised latest on the branch, backed by a `triage` event the
  monitor re-broadcasts — so it costs almost no new machinery.

The dashboard renders the mark as a second badge beside attention. When the
session moves on (its `last_activity_at` advances past `triage_at`) the mark is
shown as **stale** until the next round refreshes it — so a "looks stuck" mark
from an hour ago doesn't lie about a session that has since recovered.

This is the same discipline as [structured projects](../structured-projects.md):
**two actors must never author the same fact.** The agent owns `attention`; the
overlooker owns `triage`; the renderer shows both.

## Level-triggered, not edge-triggered

The single most important design decision, lifted straight from the Kubernetes
controller pattern. A trigger does **not** carry a payload the round reacts to.
It carries only "wake up and look." The round then:

1. **Observes** the *current* scoped fleet (list sessions, read their status,
   attention, last-activity, recent diff, PR snapshot — all existing reads).
2. **Reconciles** — decides, per session, whether the mark is still right and
   whether to act.

Why this matters:

- **Idempotent.** Running a round twice produces the same marks; re-firing is
  always safe.
- **Crash-safe.** If loom restarts and the engine misses ten events, the next
  round still sees the true current state. There is no event-replay to get
  wrong (exactly why the monitor already works on a watermark, not a queue).
- **No nag loops.** Because the round compares against current state, it only
  re-acts when something *changed*. An edge-triggered design ("on every
  `waiting` event, nudge") would hammer a session that's legitimately waiting.

Events are thus demoted to *nudges to look sooner* — a `blocked` event might
wake a round immediately instead of waiting for the next cron tick — but the
round's logic is always "survey and reconcile the whole scope," never "handle
this one event."

## Execution: reuse the session machinery

"These agents reuse our session tmux etc logic" is the explicit constraint, and
the right one. There is nothing special about *running* an overlooker — it is an
agent in a tmux pane, launched by `agent::launch`, reporting via the same hooks.
What differs is *who drives it* (the trigger engine, not a one-time launch) and
*where it shows up* (the panel, not the fleet). Three modes, picked per
overlooker:

### Warm (default for judgement)

The overlooker **is** a long-lived session — its own tmux pane, launched once,
kept alive between rounds. Each trigger **nudges** it via the existing
`send` primitive (`tmux send-keys`) with a freshly-composed context package:
"It's the 14:00 round. Here are the 3 non-idle sessions and what changed since
12:00. Investigate and mark them." The agent uses its on-PATH tools (`weaver`,
`loom session preview/send`, `gh`) to drill in, then records its marks.

Warm mode is the heartbeat pattern with memory: because the TUI persists, the
agent *remembers* it flagged session X as stuck last round and can ask "still
stuck?" — precisely the accumulating judgement the problem wants. It reuses the
session machinery to the letter: it is a normal session, just hidden from the
fleet and driven by the engine instead of a human.

### Headless (for cheap, stateless judgement)

A one-shot `claude -p` per round, exactly as
[`scripts/lint-review.py`](../lint.md) already spawns a headless reviewer:
environment stripped of `CLAUDE_CODE_*` so it neither nests in a transcript nor
bills the metered API, prompt on stdin, a timeout (lint-review uses 600 s),
output parsed. No idle tmux between rounds; no memory across rounds (the context
package carries everything). Right for "summarise the fleet each morning"-style
checks that don't need continuity.

### Script (for mechanical rules and custom triggers)

The round runs a **Python file** (see [The Python
surface](#the-python-surface-pyo3)). The script decides everything — whether to
call an LLM at all. Most fleet checks are pure rules ("idle > 30 min and PR
checks failing → mark attention") that need no model and cost nothing. The
script is also where genuinely custom trigger logic lives.

> **Recommendation:** ship **headless** first (it reuses the proven
> lint-review subprocess pattern and needs no idle-session lifecycle), make
> **warm** the default for judgement overlookers once the panel can show their
> terminal, and add **script** with the Python surface last.

## Capabilities & safety

An agent that can act on *other people's* sessions is a loaded gun; the
stuck-agent-watcher literature is unanimous that bounded, auditable autonomy is
the whole game. Each overlooker declares a capability set, least-privilege by
default — the **intervention ladder**, rung by rung:

| Capability | What it allows | Default |
|---|---|---|
| `observe` | all read APIs (preview, diff, log, PR status) | always on |
| `mark` | write the `triage` axis on a session | on |
| `escalate` | raise the *overlooker's own* attention / push a notification to the human | on |
| `nudge` | `loom session send` a message into a watched session | **opt-in** |
| `interrupt` | `loom session break` a watched session | **opt-in** |
| `launch` | spawn new sessions | **opt-in**, highest privilege |

Plus global guardrails, none of them optional:

- **Budget per round** — a wall-clock timeout (the lint-review 600 s precedent)
  and, for LLM modes, a token ceiling. A runaway round is killed, the run
  recorded `error`, the next trigger still fires.
- **Cooldown + no overlap** — a minimum gap between rounds and a hard rule that
  an overlooker never runs two rounds concurrently (a round still in flight when
  its trigger re-fires is skipped, recorded `skipped`).
- **No recursion** — an overlooker's scope can never include overlooker
  sessions, and an overlooker cannot act on another overlooker. Watchers don't
  watch watchers.
- **Everything is an event** — every mark, nudge, interrupt, and launch is
  recorded as an `events` row and shown in the round's history. Nothing the
  overlooker does is invisible or unattributable.
- **Kill switches** — a per-overlooker `enabled` toggle and a global
  `overlooker.enabled` setting; flipping either stops it cold, no redeploy.

The conservative default — observe, mark, escalate, but do not touch — means the
worst a misfiring overlooker can do out of the box is paint a wrong badge and
ping you. Touching a session is a deliberate opt-in per watcher.

## The trigger engine

A background task, `overlooker::run(state)`, spawned in `server::serve`
alongside `monitor::run` and `github::poll` — the established shape for loom's
background loops. It is a single loop that:

1. **Cron** — keeps each cron overlooker's next-fire time (parsed with the
   `croner` crate, the de-facto Rust cron parser; `tokio-cron-scheduler` builds
   on it). When `now ≥ next_fire`, it fires and recomputes.
2. **Event** — consumes `events::since(watermark)` exactly as the monitor does,
   matching new rows against each event-overlooker's filter. A match, past
   cooldown and debounce, fires a round.
3. **Dispatch** — firing creates an `overlooker_runs` row, composes the context
   package over the scope, runs the round in the configured mode, captures the
   summary and actions, and records the events.

It is a **separate** task from the monitor, not folded in: the monitor's job is
mechanical liveness, and keeping the overlooker's heavier, LLM-spawning,
fail-prone work out of that tight 1.5 s loop keeps both legible. Like the GitHub
poller it self-gates — if `overlooker.enabled` is off or no overlookers are
defined, it idles cheaply.

## The Python surface (PyO3)

"Cron triggers & workflows might be indicated via a Python script which we PyO3
bind the Loom & Weaver API into." This is the **scripted** authoring surface,
and the binding direction matters.

Ship a **`weaver` Python package built with maturin/PyO3** — a native module
that wraps loom's existing REST `client.rs` (and weaver-core's DB-direct reads).
Python *imports Rust*; it does **not** embed Python inside loom. That choice is
deliberate:

- It mirrors the pattern already in the tree: `lint-review.py` runs as a
  **subprocess**, env-stripped, with a timeout. The overlooker engine runs a
  scripted round the same way. No GIL in the server, no interpreter-embedding
  packaging pain (the PyO3 guide's "embed Python in Rust" path is the hard one).
- A maturin module is `pip install`-able, so the same script a user iterates on
  standalone is what the engine runs — it just talks to loom over the REST API
  either way.

A scripted overlooker looks like:

```python
import weaver

@weaver.on_cron("0 * * * *")           # the trigger, declared in code
def hourly_status(round):
    for s in weaver.sessions(attention="!ok"):   # the scope, queried
        screen = s.preview(lines=200)
        if looks_stuck(screen):                  # a pure rule — no LLM
            s.mark("attention", "stuck: same test failing 5×")
            s.nudge("You've retried this test unchanged 5×. Step back and re-read the error.")
        else:
            s.mark("ok", "progressing")
```

`@weaver.on_cron(...)` / `@weaver.on_event(...)` declare the trigger; the engine
reads them at registration and schedules accordingly. The `weaver.sessions(...)`
query is the scope; `s.preview/diff/log` are observe; `s.mark/nudge/break` are
the capability-gated actions (the binding enforces the overlooker's capability
set — a script with only `mark` can't call `nudge`). The script may call an LLM
itself (`claude -p`, the Anthropic SDK) for judgement, or stay pure-rules and
cost nothing.

This unifies the rules-engine and supervisor-agent views: the Python file *is*
the glue, free to be a one-line rule or a full agent harness, on the same
trigger/scheduling/capability backbone as the declarative overlooker.

> The Python surface is the **last** phase. The declarative overlooker delivers
> the headline example with zero Python; the PyO3 module is the power-user
> escape hatch, not the price of entry.

## Architecture

```mermaid
flowchart TD
    cron["cron tick (croner)"] --> engine
    ev["session events<br/>(events table)"] --> engine
    manual["Run now / loom overlooker run"] --> engine

    engine["Overlooker engine<br/>background loop, sibling of monitor + github::poll<br/>budget · cooldown · no-overlap · no-recursion"]
    engine -->|fires a round| round["Round"]

    round -->|warm: nudge| warm["Warm session<br/>tmux + agent::launch + hooks"]
    round -->|headless| headless["claude -p one-shot<br/>env-stripped subprocess"]
    round -->|script| py["Python script<br/>PyO3-bound weaver module"]

    warm --> act
    headless --> act
    py --> act
    act["observe · mark · nudge · break · escalate<br/>(capability-gated)"]

    act -->|observe| fleet["Fleet sessions<br/>(preview / diff / log / PR)"]
    act -->|mark| triage["triage axis on branch<br/>(level · note · by · at)"]
    act -->|nudge / break| fleet
    round --> runs["overlooker_runs<br/>(audit trail)"]

    triage --> panel["Overlooker panel<br/>+ mark badges on the fleet"]
    runs --> panel
```

## Data, CLI, and API sketch

**Storage — two new tables plus the `triage` axis.**

- `overlookers` — `id`, `name`, `enabled`, `trigger_kind` (`cron|event|manual`),
  `trigger_spec` (the cron expression or the event filter, JSON), `scope`
  (the fleet query, JSON), `mode` (`warm|headless|script`), `instructions`
  (prompt or script path), `model`, `effort`, `capabilities` (JSON set),
  `cooldown_secs`, `last_run_at`, `next_run_at`, `session_id` (the warm
  session, when `mode = warm`), `created_at`, `updated_at`.
- `overlooker_runs` — `id`, `overlooker_id`, `trigger_reason`, `started_at`,
  `finished_at`, `outcome` (`ok|noop|skipped|error`), `summary`, `actions`
  (JSON: the marks/nudges it made), `created_at`. The audit trail and the
  panel's history list.
- **`triage` axis** — denormalised `triage_level`, `triage_note`, `triage_by`,
  `triage_at` on `branches`, plus a `triage` event kind. Mirrors `attention`;
  reuse the monitor's re-broadcast path.

**CLI (`weaver`, DB-direct — the agent/script side):**

- `weaver triage <session> <level> "<note>"` — write the mark. The one new
  agent-facing verb; the binding and the warm/headless agent both call it.

**CLI (`loom`, the operator side) — a new `loom overlooker` group:**

- `loom overlooker ls` — list overlookers with last/next run and enabled state.
- `loom overlooker add … ` / `rm <id>` / `enable|disable <id>`.
- `loom overlooker run <id>` — fire a round now (manual trigger).
- `loom overlooker runs <id>` — round history.
- `loom overlooker logs <id>` / reuse `loom attach` for a warm overlooker's
  pane.

**API (`loom`, `web.rs` — API-first, the SPA and CLI both consume it):**

- `GET POST /api/overlookers`, `GET PATCH DELETE /api/overlookers/{id}`.
- `POST /api/overlookers/{id}/run` — manual round.
- `GET /api/overlookers/{id}/runs` — history.
- `POST /api/sessions/{id}/triage` — write the mark (backs `weaver triage` and
  the binding); `SessionView` / `BranchView` grow the `triage` fields.

**Settings (`config::registry()`):** `overlooker.enabled` (Bool, default
`false` — opt-in subsystem), `overlooker.default_timeout_secs`,
`overlooker.default_cooldown_secs`, all under an **Overlooker** group, exactly
like the GitHub group.

## The panel (loom UI)

A new top-level **Overlooker** view, sibling to the session list and Settings —
the "separate panel & system" the problem asks for. Built API-first, a thin REST
client like the rest of the SPA ([[ui-built-on-rest-api]]):

- **List** — each overlooker: name, trigger (next fire / last run), mode,
  enabled toggle, last outcome. A **Run now** button per row.
- **Detail** — the config editor (trigger, scope, capabilities, instructions);
  the **round history** (`overlooker_runs`, each with its summary and the marks
  it made); and, for a warm overlooker, its **live terminal** via the existing
  `AgentTerminal` component — you can watch it work or take it over.
- **On the fleet** — every session in the main list grows the **mark badge**
  beside its attention badge, with the `triage_note` on hover and a stale
  indicator when the session has moved since it was marked. This is where the
  overlooker's judgement actually lands in front of you.

## Worked example: the hourly status overlooker

The motivating case, end to end, in the **declarative** form (no Python):

```
loom overlooker add status-check \
  --cron "0 * * * *" \
  --scope "attention != ok" \
  --mode warm --model sonnet \
  --capabilities observe,mark,escalate,nudge \
  --instructions "For each session, read its recent screen and diff. If it's \
    making progress, mark it ok. If it looks stuck (same action repeating, an \
    unhandled error, no movement), mark it attention with a one-line reason and \
    nudge it with a concrete next step. If it needs a human decision, mark it \
    blocked and escalate."
```

At the top of each hour the engine wakes the warm `status-check` session,
sends it the round's context package (the non-idle sessions and what changed
since last hour), and the agent walks them: `loom session preview` to see each
screen, `weaver triage` to mark, `loom session send` to nudge the stuck one,
and — if three need you — raises its own attention so the dashboard flags
*the overlooker* with "3 sessions need you." A `blocked` event from any session
in scope wakes the round early instead of waiting for the hour. You open the
Overlooker panel, read the round summary, and see the marks on your fleet.

## Tasks

Each task has a stable id (`T1`, `T2`, …). `exec: session` tasks materialise
into weaver issues on `weaver plan sync overlooker`; status is projected from the
issue ledger, never hand-edited here. The dependency edges (`deps:`) form the
delivery order — each task is independently shippable once its deps land.

### T1 — The mark axis (triage)  `exec: session`  `value: high`  `deps: —`

The third status axis, in isolation. Add `triage_level` / `triage_note` /
`triage_by` / `triage_at` to `branches`, a `triage` event kind the monitor
re-broadcasts (mirror the `attention` path), the agent-facing `weaver triage
<session> <level> "<note>"` CLI, the `POST /api/sessions/{id}/triage` route, the
`triage` fields on `SessionView` / `BranchView`, and the **mark badge** beside
the attention badge on the fleet — with the stale indicator when the session has
moved past `triage_at`. Acceptance: a human (or a script) can mark a session and
see the badge, with the agent's own `attention` untouched.

### T2 — Headless round + manual trigger  `exec: session`  `value: high`  `deps: T1`

The `overlookers` table and a single overlooker fired only by `loom overlooker
run <id>` — no scheduler yet. The round composes a context package over a scope
query and runs a `headless` round (the env-stripped `claude -p` subprocess
pattern from [`scripts/lint-review.py`](../lint.md), with a timeout), whose marks
land via T1. Acceptance: `loom overlooker run` produces marks on the scoped
fleet end to end.

### T3 — Trigger engine: cron + guardrails  `exec: session`  `value: high`  `deps: T2`

The `overlooker::run` background task (sibling of `monitor::run` /
`github::poll`, self-gating on `overlooker.enabled`), cron firing via `croner`
with next-fire bookkeeping, and the non-optional guardrails: per-round wall-clock
timeout + token budget, cooldown, no-overlap (a still-running round skips its
re-fire), and the no-recursion rule (an overlooker never scopes or acts on
overlooker sessions). The `overlooker.*` settings in `config::registry()`.
Acceptance: the hourly status overlooker runs unattended within its budget.

### T4 — Event triggers  `exec: session`  `value: med`  `deps: T3`

Level-triggered event firing: consume `events::since(watermark)` like the
monitor, match new rows against an overlooker's event filter (e.g.
attention→`blocked`, staleness), debounce, and wake a round *early* — the event
is a nudge to re-survey the whole scope, never a payload the round reacts to once.
Acceptance: a `blocked` event wakes a round before the next cron tick, and
re-firing is idempotent.

### T5 — Round history + audit  `exec: session`  `value: med`  `deps: T2`

The `overlooker_runs` table: every round's trigger reason, outcome
(`ok|noop|skipped|error`), summary, and the actions it took — each action also
an `events` row. The audit trail the safety story and the panel depend on.
Acceptance: every round and every action it took is reconstructable after the
fact.

### T6 — The Overlooker panel (UI)  `exec: session`  `value: high`  `deps: T3, T5`

A new top-level **Overlooker** view, API-first and a thin REST client
([[ui-built-on-rest-api]]): the list (trigger, next/last run, enabled toggle,
Run now), the detail (config editor for trigger/scope/capabilities/instructions,
plus the round history from T5), the `loom overlooker` CLI group, and the
`/api/overlookers*` routes. Surfaces the T1 mark badge on the main fleet list.
Acceptance: an overlooker can be created, edited, enabled, and run from the UI,
with its history visible.

### T7 — Warm mode  `exec: session`  `value: med`  `deps: T3, T6`

Long-lived nudged overlooker sessions: launch once via `agent::launch`, keep the
tmux warm, and nudge each round through the existing `send` primitive with the
round's context package — accumulating judgement across rounds. The live
terminal in the panel via the `AgentTerminal` component, and the warm session's
orphan/adopt lifecycle. Acceptance: a warm overlooker remembers a prior round's
mark and refines it, and is attachable from the panel.

### T8 — The Python surface (PyO3)  `exec: session`  `value: low`  `deps: T3, T5`

The maturin/PyO3 `weaver` module wrapping the REST `client.rs`, and `script`-mode
overlookers run as env-stripped subprocesses on the engine. `@weaver.on_cron` /
`@weaver.on_event` decorators declare the trigger; `weaver.sessions(...)` is the
scope; `s.mark` / `s.nudge` / `s.break_` are capability-gated by the binding (a
script without `nudge` cannot call it). Acceptance: a pure-rules Python
overlooker and an LLM-calling one both run on the engine with capabilities
enforced.

## Non-goals

- **Not a second agent runtime.** Overlookers run on the existing
  `agent::launch` + tmux + hooks + `loom session` primitives. We add a scheduler
  and a panel, not a parallel way to run agents.
- **Not edge-triggered automation.** No "on event X, do Y once." Triggers are
  nudges to re-survey; rounds reconcile current state. (The K8s lesson; the
  anti-pattern is the nag loop.)
- **Not autonomous remediation by default.** Out of the box an overlooker
  observes, marks, and escalates. Touching sessions (nudge/interrupt/launch) is
  a deliberate per-watcher opt-in, never the default.
- **Not the agent's self-report.** The mark is the overlooker's opinion; it
  never overwrites `attention`. Two actors, two axes.
- **Not embedded Python.** The PyO3 binding is a `pip`-installable module Python
  imports and the engine runs as a subprocess — not an interpreter embedded in
  the loom server.

## Open questions

- **Warm-session lifecycle.** A warm overlooker is an idle tmux session between
  rounds. Does it survive a loom restart via the existing orphan/adopt path
  (likely yes — it's a normal session), and should the engine auto-adopt its
  warm sessions on startup regardless of the global `server.auto_adopt`?
- **Scope query language.** How rich does the `--scope` filter need to be before
  it wants the Python surface? Start with a small fixed set (attention, status,
  repo, staleness) and let `script` mode cover the rest.
- **Cron vs interval ergonomics.** Expose raw crontab (`croner`), a friendly
  `--every 1h`, or both? Both is cheap; pick the default for the UI.
- **Escalation channel.** `escalate` raising the overlooker's own `attention` is
  free and needs no new surface. A real push notification (the deferred
  `PushNotification` capability) is a richer follow-up — worth it once the panel
  exists.

## Sources

Supervisor / multi-agent orchestration:
[LangGraph supervisor vs swarm](https://focused.io/lab/multi-agent-orchestration-in-langgraph-supervisor-vs-swarm-tradeoffs-and-architecture),
[Databricks supervisor architecture](https://www.databricks.com/blog/multi-agent-supervisor-architecture-orchestrating-enterprise-ai-scale),
[multi-agent orchestration patterns](https://lushbinary.com/blog/multi-agent-orchestration-patterns-supervisor-swarm-pipeline-router-guide/).
Scheduled / heartbeat agents:
[heartbeat pattern](https://www.mindstudio.ai/blog/what-is-heartbeat-pattern-paperclip-ai-agents),
[cron-based AI automation](https://www.mindstudio.ai/blog/build-cron-based-ai-automation-hermes-agent),
[self-running agents with heartbeat, cron & memory](https://dev.to/linou518/autonomous-ai-agents-building-self-running-ai-with-heartbeat-cron-memory-14g9).
Reconciliation / level-triggered:
[the principle of reconciliation](https://www.chainguard.dev/unchained/the-principle-of-reconciliation),
[reconciliation loop (kubebuilder)](https://deepwiki.com/kubernetes-sigs/kubebuilder/5.2-reconciliation-loop),
[the reconciler pattern](https://www.farishuskovic.dev/blog/k8s-reconciler-pattern/).
Rules engines / event-driven automation:
[automation rules: triggers, conditions, actions](https://chatboq.com/blogs/automation-rules),
[rule engine vs workflow engine](https://www.nected.ai/blog/rule-engine-vs-workflow-engine).
Stuck-agent intervention / HITL:
[Wink: recovering from coding-agent misbehaviours](https://arxiv.org/pdf/2602.17037),
[stopping AI agent loops](https://markaicode.com/fix-ai-agent-looping-autonomous-coding/),
[human-in-the-loop agentic systems](https://medium.com/@tahirbalarabe2/human-in-the-loop-agentic-systems-explained-db9805dbaa86).
Implementation building blocks:
[PyO3](https://github.com/PyO3/pyo3) and the [calling-Python / embedding guide](https://pyo3.rs/),
[croner (Rust cron parser)](https://github.com/Hexagon/croner-rust),
[tokio-cron-scheduler](https://github.com/mvniekerk/tokio-cron-scheduler).
