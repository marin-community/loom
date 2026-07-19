# The loom UI design language

How the loom SPA looks and why — the design system the views are built
against. Architecture (routes, API, build) lives in
[ARCHITECTURE.md](ARCHITECTURE.md); this file owns the visual rules. The
direction in one line: **a reading room, not a dashboard** — the density and
edge-to-edge discipline of an instrument panel, dressed in the warmth of an
academic study: warm paper, a serif prose voice, iron-gall ink for the one
accent, and a muted naturalist's palette for signal. Not a cold VS Code
workbench, not a centered bootstrap template.

## Principles

- **Edge-to-edge workbench.** A persistent left rail, full-bleed content, a
  thin status bar. No centered `max-w` container, no tall header.
- **Chrome recedes, content advances.** The rail and status bar sit on the
  recessed surface and stay quiet; the one accent — iron-gall ink (a muted
  blue-black, the colour of a fountain pen, not SaaS blue) — is reserved for the
  active view, focus, links, and the primary action.
- **Warm paper, not clinical gray.** Both palettes are warm: light is a
  low-chroma stone paper (archival card stock, deliberately *not* cream) with
  warm near-black ink; dark is a warm charcoal read by lamplight, not a neutral
  void. The warmth is what lets a long day rest the eye.
- **Three typographic voices.** A literary **serif** carries human prose
  (session titles, goals, issue titles, status notes, conversation, markdown); a
  quiet **sans** is the UI chrome (toolbars, labels, buttons); the **mono** is
  everything machine-made. The reader always knows, by the letterforms alone,
  whether they're reading a person, the interface, or the machine.
- **Borders, not shadows.** Regions separate with 1px `--line` hairlines.
  Shadows belong only to true overlays (popovers, dropdowns).
- **Mono for everything machine-made.** Ids, branch refs, paths, timestamps,
  counts, statuses — `--font-mono`, usually at 11–12px, with
  `tabular-nums` so live numbers don't jump.
- **One loud signal, a calm spectrum below it.** Ochre (attention) and oxblood
  (blocked) are the only *loud* colors, reserved for the resolved attention
  axis — warmed toward pigment so they stay urgent without glare. Below them sits
  a naturalist's palette — sage green (`--ok`), slate-teal (`--info`), dusk plum
  (`--agent`) — each muted a step further than the loud axis, so lifecycle, PR
  state, idle and model-tier read with scannable color without ever out-shouting
  a session that needs a human, and none of it reads as crayon. Tags and
  free-form metadata stay neutral.
- **Instant, not animated.** An instrument panel shows content, it doesn't
  perform it in. Views are kept alive across navigation, so returning to the
  fleet or a session is instant — no remount, no refetch flash, no replayed
  entrance. There is no per-row entrance animation; the one motion is a single
  0.12s opacity settle (`fade-in`) on a region's first paint.

## App shell

```
┌──┬──────────────────────────────────────────────┐
│  │                                              │
│ r│   view content (each view owns a compact     │
│ a│   one-line toolbar: small-caps title,        │
│ i│   counts, filters, primary action)           │
│ l│                                              │
│  ├──────────────────────────────────────────────┤
│  │ status bar: fleet vitals · clock        24px │
└──┴──────────────────────────────────────────────┘
 56px
```

- **Rail** (56px, `--rail` background, 1px right border): the wordmark up
  top, then Sessions / Issues / Watches as icon+label items; Settings and
  the theme toggle pinned at the bottom (the VS Code idiom). The active view
  gets a 2px accent bar on the rail item's left edge plus full-strength icon;
  inactive items sit muted. There is **no top app bar** — the rail is the nav,
  each view's first line is its toolbar.
- **Status bar** (24px, mono 11px): live fleet vitals on the left —
  `N sessions · M need attention`, the attention segment amber and clickable
  (jumps to the filtered list) when non-zero — and a `HH:MM:SS` clock plus
  connection dot on the right. Data comes from the same `/api/sessions` the
  list polls; the bar is read-only API state, never browser-local truth.
- **View toolbars**: one ~32px line — a small-caps 11px `<h1>` (kept for
  accessibility and tests), count pill, view filters, and the primary action
  pushed right. Detail pages (session, watch) keep their richer headers.

## Color

Semantic tokens only (`bg-surface`, `text-muted`, `border-line`, …) — never
raw Tailwind palette colors in views. Both palettes are warm — a reading room,
not VS Code:

- **Light** ("by daylight"): canvas `#eceadf` (warm stone paper), surface
  `#f7f5ee`, rail `#e3e0d4`, hairline `#dbd7c9`, ink `#26221a` / muted `#5f584b`
  / faint `#8c8574`, accent `#345b7d` (iron-gall ink).
- **Dark** ("by lamplight"): canvas `#1b1915` (warm charcoal), surface
  `#232019`, rail `#151310`, hairline `#332f27`, text `#e9e3d6` (warm
  paper-white) / muted `#a49b8b` / faint `#726a5c`, accent `#7ea6c6` (lifted
  iron-gall ink). The embedded terminal pane sits on `#141210` so it reads as a
  recessed panel, not a black hole.
- **Attention axis**: ochre `attn-*` / oxblood `block-*` tokens, soft row washes
  + 2px left accent line — the only loud fills, warmed toward pigment.
- **Calm semantic hues**: `ok-*` (sage green — healthy / live / passing),
  `info-*` (slate-teal — resting / neutral-positive), `agent-*` (dusk plum —
  model / AI identity). Each carries the hue plus a `-soft` wash and a `-line`
  for dots/hairlines, and each is muted a step further than the loud axis. They
  tint the list's per-row status dot, lifecycle badges, GitHub PR state, the idle
  chip, the "▶ Working" / "all calm" cues, and the model tier — a naturalist's
  plate, not a crayon box: color that *means* something, so the fleet reads at a
  glance and a raised signal still wins the eye.

## Type & density

- **Fonts** — three voices, bundled via `@fontsource` (self-hosted variable
  woff2, no CDN, system stacks as fallback):
  - `--font-serif` **Source Serif 4** (optical-size axis) — the app's prose
    voice: session titles, goals, issue titles, status notes, conversation, and
    rendered markdown. The optical axis means a 19px title and an 11px caption
    are each cut for their size.
  - `--font-sans` **Source Sans 3** — the quiet UI chrome: toolbars, labels,
    buttons, micro-caps. It stays out of the serif's way.
  - `--font-mono` **IBM Plex Mono** — every machine identifier, and the xterm
    terminal, so the terminal and the metadata around it share one voice.
- **Scale**: prose is set in the serif (row title 15px, detail title 19px, goal/
  status note 13px, markdown 15px); UI chrome is 13px sans (controls), 12px
  secondary, 11px (`text-2xs`) uppercase micro-labels
  (`font-medium uppercase tracking-wider text-muted`) and all badge/chip text.
  The biggest text in the app is a detail-page title at 19px.
- **Density**: rows `px-3 py-2` (~36px), panels `p-4`, controls 28px tall,
  radius 4px on controls and 6px on panels. The 4/8/12/16 spacing grid.
- **Numbers**: `tabular-nums` globally.

## Fleet list: order & the resting shelf

The fleet list is where a long day's fatigue accumulates, so two controls keep it
from becoming a wall of stale rows:

- **The resting shelf ("Parked").** Below the live list sits a collapsed shelf
  for threads that need nothing from you right now — hand-parked, or simply long
  idle (the agent has rested past `IDLE_PARK_HOURS` — hours, not minutes, so a
  finished turn never parks a conversation, only an abandoned one). Shelf rows
  are dimmed, labelled with *why* they rest (`idle 12h` / `idle 6d` / `parked`),
  and one click (or a drag) away from live. A loud signal always keeps a thread
  live — a session that needs a human never hides. A session merely *awaiting an
  external reviewer* is **not** shelved: its `awaiting: review` mark sinks it
  below the calm rows in the live list (it's still yours to glance at) but never
  hides it away. The idle threshold is a pure client view over
  `last_activity_at`; only the manual override (`park`: `'parked'` / `'active'`)
  is persisted.
- **Manual order.** A hover-revealed grip (`⠿`) drags a top-level thread to
  reorder it, or onto the shelf to rest it (drag back out, or "Keep live", to
  return it). A drag persists one midpoint `sort_order`; placed and untouched
  rows share one numeric axis (`orderKey`), so a dragged row lands exactly where
  dropped while every other row keeps its automatic urgency-then-recency spot.
  The grip is the only draggable handle, so the row's link still click/⌘-clicks.

## Recurring pieces

- `meta-chip` / `tag-pill` / `pill` utilities (styles.css) for mono metadata,
  free-form tags, and neutral counts; `btn-primary` / `btn-secondary` /
  `btn-danger` for every button — no hand-rolled button class runs in views.
- All badges (lifecycle, outcome, issue status) share one recipe: 11px mono
  uppercase, `px-1.5 py-0.5`, radius 4, neutral fill. The attention badge is
  the only filled loud chip.
- `ToggleSwitch` is the one boolean control (watch enable, bool
  settings).
- Focus: a crisp 1px accent `:focus-visible` outline, never removed, no glow.
- Hover: background shift only (`--subtle`), ≤150ms, no lift/scale.
- Row actions that repeat per row (issue close/edit/delete) are
  hover/focus-revealed ghost buttons, not an always-on button row.
- Empty states: a bordered dashed card with one muted line and, where
  sensible, the CTA; never a bare floating sentence.
- **The ACP Conversation surface** (`AcpConversation.vue`, for
  `protocol='acp'` sessions) is typeset dialogue, not chat bubbles: each turn
  opens with a hairline speaker rule — a micro-caps sans label (`YOU`
  accent-coloured / `AGENT`) + a mono timestamp — over serif prose (the shared
  `MarkdownView`); no avatars, no left/right alternation. The machine's
  apparatus recedes into folds: *every* run of consecutive tool calls — quiet
  reads and consequential edits alike — collapses to one mono activity line
  (`▸ ✎ Edit web.rs`, or `▸ 5 steps — 3 read · 1 edit · 1 execute` for a run),
  closed by default; opening it lists the calls, and a call with output opens
  further to its payload on the recessed `--code` tone (diffs as ±lines,
  command output as a clamped mono block). A failed call is the exception that
  announces itself: its group opens by default with an oxblood `N failed`
  badge and the failing call's output showing. The one interactive block is
  the permission card (ochre `--attn-line` rule + `--attn-soft` wash, sans
  option buttons, collapsing to a mono receipt once answered). Turns close
  with a dashed hairline (`turn N · stop_reason · NNk ctx`). A live turn ends
  the transcript with a status line naming what the agent is doing right now —
  the running tool's title, `Thinking…` while reasoning streams (its live tail
  showing above, top-faded, before folding away), `Writing…` while prose
  streams — set in a soft text shimmer (the one licensed animation beyond
  `fade-in`, stilled under `prefers-reduced-motion`) beside a mono
  `turn N · M:SS` elapsed count. An empty conversation states itself: a dashed
  card ("No conversation yet") instead of a bare canvas. Both conversation
  surfaces (ACP and the terminal-log tab) open **pinned to the foot** — the
  newest exchange — and follow growth while the reader stays there; scrolling
  up releases the pin, scrolling back to the foot re-arms it (a ResizeObserver
  on the transcript body, so async markdown paints can't strand the view
  mid-history). The right rail
  carries the user-turn jump list plus the current `plan` checklist (✓/▸/○ in
  ok/agent/faint), folding away at narrow widths. The composer is a serif
  input with a mono mode chip (`bypass ▾` → `session/set_mode`) on the left
  and Stop/Send on the right.

## Follow-ups (not yet done)

- A `data-density="compact"` toggle (32→28px rows) persisted in settings.
- A `⌘K` command palette over sessions/issues.
- Middle-ellipsis truncation for long ids/refs.
