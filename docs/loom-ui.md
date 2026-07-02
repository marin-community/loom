# The loom UI design language

How the loom SPA looks and why — the design system the views are built
against. Architecture (routes, API, build) lives in
[ARCHITECTURE.md](ARCHITECTURE.md); this file owns the visual rules. The
direction in one line: **an instrument panel, not a website** — VS Code's
workbench and Linear's density, not a centered bootstrap template.

## Principles

- **Edge-to-edge workbench.** A persistent left rail, full-bleed content, a
  thin status bar. No centered `max-w` container, no tall header.
- **Chrome recedes, content advances.** The rail and status bar sit on the
  darkest surface and stay near-monochrome; the one saturated accent (blue) is
  reserved for the active view, focus, links, and the primary action.
- **Borders, not shadows.** Regions separate with 1px `--line` hairlines.
  Shadows belong only to true overlays (popovers, dropdowns).
- **Mono for everything machine-made.** Ids, branch refs, paths, timestamps,
  counts, statuses — `--font-mono`, usually at 11–12px, with
  `tabular-nums` so live numbers don't jump.
- **One loud signal, a calm spectrum below it.** Amber (attention) and red
  (blocked) are the only *loud* colors, reserved for the resolved attention
  axis. Below them sits a calm semantic palette — green (`--ok`), cyan
  (`--info`), violet (`--agent`) — each lower-saturation than the loud axis, so
  lifecycle, PR state, idle and model-tier read with scannable color without
  ever out-shouting a session that needs a human. Tags and free-form metadata
  stay neutral.
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
raw Tailwind palette colors in views. Both palettes are neutral (VS Code
Modern–derived), not blue-tinted slate:

- **Dark**: canvas `#1f1f1f`, surface `#252526`, rail/chrome `#161616`,
  hairline `#2b2b2b`, text `#e4e4e4` / muted `#9d9d9d` / faint `#6e6e6e`,
  accent `#3b82f6`. The embedded terminal pane sits on `#181818` so it reads
  as a recessed panel, not a black hole.
- **Light**: canvas `#f5f5f5`, surface `#ffffff`, rail `#ebebeb`, hairline
  `#e0e0e0`, text `#1f1f1f` / muted `#5a5a5a` / faint `#8c8c8c`, accent
  `#005fb8`.
- **Attention axis**: amber `attn-*` / red `block-*` tokens, soft row washes +
  2px left accent line — the only loud fills.
- **Calm semantic hues**: `ok-*` (green — healthy / live / passing), `info-*`
  (cyan — resting / neutral-positive), `agent-*` (violet — model / AI
  identity). Each carries the hue plus a `-soft` wash and a `-line` for
  dots/hairlines, and each is a step softer than the loud axis. They tint the
  list's per-row status dot, lifecycle badges, GitHub PR state, the idle chip,
  the "▶ Working" / "all calm" cues, and the model tier — color that *means*
  something, so the fleet reads at a glance and a raised signal still wins the
  eye.

## Type & density

- **Fonts**: IBM Plex Sans (UI) + IBM Plex Mono (machine text), bundled via
  `@fontsource` — self-hosted with the app, no CDN, system stacks as
  fallback. The xterm terminal uses the same mono so the terminal and the
  metadata around it share one voice.
- **Scale**: 13px is the workhorse UI size (row titles, controls); 12px
  secondary text; 11px (`text-2xs`) for uppercase micro-labels —
  `font-medium uppercase tracking-wider text-muted` — and all badge/chip
  text. Page-hero type sizes don't exist; the biggest text in the app is a
  detail-page title at 16px.
- **Density**: rows `px-3 py-2` (~36px), panels `p-4`, controls 28px tall,
  radius 4px on controls and 6px on panels. The 4/8/12/16 spacing grid.
- **Numbers**: `tabular-nums` globally.

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

## Follow-ups (not yet done)

- A `data-density="compact"` toggle (32→28px rows) persisted in settings.
- A `⌘K` command palette over sessions/issues.
- Middle-ellipsis truncation for long ids/refs.
