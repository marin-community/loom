import type { Session, Tag } from '../types';

export type Attention = 'ok' | 'attention' | 'blocked';

// Loudness lives in the tag VALUE, not the key. Any tag whose value is on this
// ladder raises a badge — regardless of key — so agents and watches both add
// loud tags without a hardcoded key registry. A tag's KEY is its type (the chip
// label); its VALUE is the severity. Every other value is a quiet, free-form
// pill. Mirrors weaver-core's `ATTENTION_VALUES`.
const SEVERITY: Record<string, number> = { attention: 1, blocked: 2 };

// The quiet mirror of SEVERITY: values that PARK a row *below* the calm default
// in the fleet sort. A parked session is waiting on an external actor (a human
// PR reviewer, …) and needs nothing from the user, so the dashboard sinks it
// under the live-but-calm rows a scanning user should look at first. Like
// loudness, parking is value-driven — any key holding such a value parks, so a
// watch picks its own axis key (e.g. `awaiting`) and the value carries the
// meaning. Quiet by design (never a badge); the two ladders are disjoint.
// Mirrors weaver-core's `PARKED_VALUES`.
const PARKED: Record<string, number> = { review: -1 };

// Sort rank below the calm default for a parked value, or 0 if it doesn't park.
function parkOf(value: string | undefined): number {
  return (value && PARKED[value]) || 0;
}

// The soothing, quiet `idle` mark loom stamps when an agent goes quiet (a
// finished turn or a `waiting` lull): the calm "resting, no one needed" state.
// It carries the quiet value `idle`, so it is never loud — an idle agent no
// longer reads as needing the user. The status watch may replace it with a real
// loud status. Mirrors weaver-core's `IDLE_KEY`.
export const IDLE_KEY = 'idle';
export const AUTO_ARCHIVE_KEY = 'auto-archive';
export const AUTO_ARCHIVE_DISABLED_VALUE = 'disabled';

/** Whether this session carries the exact user-controlled retention opt-out. */
export function autoArchiveDisabled(s: Session): boolean {
  return (s.branch.tags ?? []).some(
    (t) => t.key === AUTO_ARCHIVE_KEY && t.value === AUTO_ARCHIVE_DISABLED_VALUE,
  );
}

// Loom's machine bookkeeping for its GitHub/Slack side-effects (the PR
// back-link mark, the status-card comment/message id). Not user-meaningful,
// so the pill row hides them; the `github`/`slack` wiring tags themselves
// stay visible.
const BOOKKEEPING_KEYS = ['github.linked', 'github.status_comment', 'slack.status_message'];

// Severity of a tag value: 0 is quiet (a pill), >0 is loud (a badge).
function severityOf(value: string | undefined): number {
  return (value && SEVERITY[value]) || 0;
}

// A loud tag's value, narrowed to the ladder (loud tags only — callers guard
// with severityOf first).
function levelValue(value: string): Exclude<Attention, 'ok'> {
  return value === 'blocked' ? 'blocked' : 'attention';
}

// An agent's own self-report vs an outside mark (a watch/watch, or a
// manual mark). The agent authors the well-known `attention` key; anything else
// loud is an outside assessment, rendered with the ⊙ "watched" treatment.
function isAgentTag(tag: Tag): boolean {
  return tag.key === 'attention' || tag.set_by === 'agent';
}

// The current-state message (Branch.description). Suppressed for archived
// sessions so torn-down workstreams don't show stale chatter.
export function messageOf(s: Session): string {
  if (s.status === 'archived') return '';
  return s.branch.description ?? '';
}

// Whether a tag predates the session's latest activity — the session has "moved
// on" since the tag was set, so an outside mark may no longer hold. The badge
// renders this faded with a stale hint. No tag, or no activity timestamp, is
// never stale.
export function tagStale(tag: Tag | undefined, lastActivityAt: string): boolean {
  if (!tag || !tag.set_at || !lastActivityAt) return false;
  return lastActivityAt > tag.set_at;
}

// The loud tags on a session (value on the ladder), archived shown none.
function loudTags(s: Session): Tag[] {
  if (s.status === 'archived') return [];
  return (s.branch.tags ?? []).filter((t) => severityOf(t.value) > 0);
}

// Who raised the resolved attention signal: the agent's own self-report, or an
// outside assessment. The pages render the agent's signal as the plain loud
// badge and an outside mark with the ⊙ "watched" treatment.
export interface EffectiveAttention {
  level: Attention;
  /** Which axis is loudest: 'agent' (its own loud tag) or 'watch' (an
   *  outside mark). 'none' when calm. */
  raisedBy: 'none' | 'agent' | 'watch';
  /** The `set_by` of the loudest tag (the watch name, or 'agent'). */
  by: string;
  /** The key of the loudest tag — its type, e.g. 'attention', 'review'. */
  key: string;
  /** One-line reason from the loudest tag. */
  note: string;
  /** True when an outside mark is the loudest signal but the session has moved
   *  on since it was set, so it should fade. */
  stale: boolean;
}

// Severity-then-agent ordering: the louder tag wins; on a tie the agent's own
// self-report leads (its self-report is the primary signal).
function louder(a: Tag, b: Tag): number {
  const d = severityOf(b.value) - severityOf(a.value);
  if (d !== 0) return d;
  return Number(isAgentTag(b)) - Number(isAgentTag(a));
}

// The attribution a loud tag carries: its type (key), severity (level), author
// (by/raisedBy) and reason (note, with the agent's message as fallback).
// effectiveAttention and signalChips both build on this so the rules stay in one
// place; they differ only in how each treats staleness.
function markOf(s: Session, tag: Tag): Omit<SignalChip, 'stale'> {
  const agent = isAgentTag(tag);
  return {
    key: tag.key,
    level: levelValue(tag.value),
    by: tag.set_by || (agent ? 'agent' : 'watch'),
    raisedBy: agent ? 'agent' : 'watch',
    note: tag.note || (tag.key === 'attention' ? messageOf(s) : ''),
  };
}

// The single resolved attention signal the dashboard renders: the loudest of a
// session's loud tags. A *non-stale* mark always beats a stale one (the session
// has moved on past a stale mark); a stale mark surfaces, faded, only when it is
// the lone signal — so an hour-old "looks stuck" fades rather than lies.
export function effectiveAttention(s: Session): EffectiveAttention {
  const calm: EffectiveAttention = {
    level: 'ok',
    raisedBy: 'none',
    by: '',
    key: '',
    note: '',
    stale: false,
  };
  const tags = loudTags(s);
  if (tags.length === 0) return calm;

  const live = tags.filter((t) => !tagStale(t, s.last_activity_at));
  const pool = live.length ? live : tags;
  const stale = live.length === 0;
  const top = [...pool].sort(louder)[0];

  return { ...markOf(s, top), stale };
}

// Whether a session sits on the PARKED sort ladder: calm (no loud signal) yet
// carrying a tag whose value parks — work waiting on an external actor (a human
// PR reviewer, …) that needs nothing from the user. This only *sinks* the row
// below the calm default in the fleet sort (`priorityRank`); it does not shelve
// it (see `parkReason`). A loud signal always wins: a session that needs a human
// never sinks, however else it's tagged. Archived sessions never sink (they read
// through their own terminal badge).
export function isParked(s: Session): boolean {
  if (s.status === 'archived') return false;
  if (loudTags(s).length > 0) return false;
  return (s.branch.tags ?? []).some((t) => parkOf(t.value) < 0);
}

// The fleet-sort rank of a single session: the loud ladder raises a row (blocked
// 2 > attention 1) via the resolved attention signal; a parked row sinks below
// the calm default (-1); everything else is the calm default (0). SessionList
// floats a thread to the max rank across its subtree, so a parked parent with
// live children stays put — only a wholly-parked thread sinks to the bottom.
export function priorityRank(s: Session): number {
  const lvl = effectiveAttention(s).level;
  if (lvl === 'blocked') return 2;
  if (lvl === 'attention') return 1;
  return isParked(s) ? -1 : 0;
}

// ---------------------------------------------------------------------------
// The resting shelf ("Parked") + manual order
// ---------------------------------------------------------------------------

// How long an agent may rest before the fleet list quietly shelves its row —
// long enough that a finished turn (idle in minutes) never parks a conversation,
// only a genuinely abandoned one does. The list is a projection of REST state
// (docs/loom-ui.md): this threshold is a pure view concern, applied client-side
// over the row's `last_activity_at`, never a stored flag — only the *manual*
// park override (`park`) is persisted.
export const IDLE_PARK_HOURS = 8;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

// Milliseconds since the agent last did anything (its `last_activity_at`, or the
// creation time as a floor). Recomputed each poll tick, so idle rows drift onto
// the shelf on their own within a poll interval.
export function idleMs(s: Session): number {
  const last = Date.parse(s.last_activity_at || s.created_at);
  if (Number.isNaN(last)) return 0;
  return Math.max(0, Date.now() - last);
}

export type ParkReason = 'manual' | 'idle';

// Why a session rests on the shelf, or `null` if it belongs in the live list.
// The shelf hides only what genuinely needs nothing from you *and* one of:
//   • you parked it by hand                        → 'manual'  (park === 'parked')
//   • the agent has rested past the idle threshold → 'idle'    (IDLE_PARK_HOURS)
// A review-wait mark deliberately does NOT shelve: an open PR awaiting an
// external reviewer is still yours to glance at, so it stays in the live list —
// sunk below the calm rows by `priorityRank` and labelled with its quiet
// `awaiting: review` pill — rather than hidden away the instant a turn ends.
// A loud signal always keeps a row live (you need to see it), and an explicit
// 'active' override pins a row live even when idle.
export function parkReason(s: Session): ParkReason | null {
  if (s.status === 'archived') return null;
  if (effectiveAttention(s).level !== 'ok') return null; // needs a human → live
  if (s.park === 'active') return null; // kept live by hand
  if (s.park === 'parked') return 'manual';
  if (idleMs(s) >= IDLE_PARK_HOURS * HOUR_MS) return 'idle';
  return null;
}

export function shelved(s: Session): boolean {
  return parkReason(s) !== null;
}

// A short mono label for the shelf badge — what kind of rest this is.
export function parkLabel(s: Session): string {
  const reason = parkReason(s);
  if (reason === 'idle') {
    const ms = idleMs(s);
    // Shelved rows have rested at least IDLE_PARK_HOURS, so hours reads first;
    // multi-day rests round up to days ("idle 6d").
    if (ms >= DAY_MS) return `idle ${Math.floor(ms / DAY_MS)}d`;
    return `idle ${Math.floor(ms / HOUR_MS)}h`;
  }
  return 'parked';
}

// The numeric key a top-level thread sorts by. A manual `sort_order` (assigned as
// the midpoint of its neighbours on drag) places the row exactly; absent, it
// falls back to the automatic order — urgency first (blocked, then attention),
// then newest. Both live on one ascending axis (smaller = higher), so a dragged
// row lands where dropped while every untouched row keeps its automatic spot.
// `subtreeRank` is the thread's loudest member (SessionList floats a thread to
// its max), so a thread with a blocked child rises as a whole.
export function autoOrderKey(s: Session, subtreeRank: number): number {
  const created = Date.parse(s.created_at) || 0;
  return -subtreeRank * 1e15 - created;
}

export function orderKey(s: Session, subtreeRank: number): number {
  return s.sort_order ?? autoOrderKey(s, subtreeRank);
}

// One loud tag surfaced as an individually-dismissable chip. Unlike
// effectiveAttention (which resolves the single loudest level for filtering,
// sorting and counts), this surfaces EACH loud tag so a human can clear them
// independently: the agent's own `attention` and a watch's typed marks are
// separate rows, each gets its own × . Clearing a chip DELETEs that tag — the
// calm state is its absence, so there is no "Mark OK" verb, just the same
// chip-delete gesture as any quiet tag.
export interface SignalChip {
  /** The tag key to DELETE when the chip is cleared, and the chip's type label. */
  key: string;
  level: Exclude<Attention, 'ok'>;
  /** Who set it: 'agent', or the watch's / mark's name. */
  by: string;
  /** Which axis: 'agent' (its own loud tag) or 'watch' (an outside mark). */
  raisedBy: 'agent' | 'watch';
  /** One-line reason from the tag. */
  note: string;
  /** An outside mark the session has moved on past — shown faded, still clearable. */
  stale: boolean;
}

// The loud signals on a session as dismissable chips, in severity-then-agent
// order. Each loud tag renders as its own chip so any one can be cleared on its
// own — which is the whole point: a stale mark is no longer a thing you "can't
// resolve", it's just a chip with an × . Archived sessions show none.
export function signalChips(s: Session): SignalChip[] {
  return loudTags(s)
    .slice()
    .sort(louder)
    .map((t) => ({ ...markOf(s, t), stale: tagStale(t, s.last_activity_at) }));
}

// The session's quiet tags: every tag whose value is not loud, for the pill row.
// These are free-form, deletable annotations (priority, needs-rebase, …). The
// soothing `idle` mark is excluded — it is a lifecycle signal surfaced calmly by
// idleTag/conversationState, not a free-form pill. Archived sessions show none.
export function quietTags(s: Session): Tag[] {
  if (s.status === 'archived') return [];
  return (s.branch.tags ?? []).filter(
    (t) => severityOf(t.value) === 0 && t.key !== IDLE_KEY && !BOOKKEEPING_KEYS.includes(t.key),
  );
}

// The soothing `idle` mark to surface, or null. Present only when the session is
// a *live* agent resting between turns: a terminal/detached lifecycle (done,
// error, archived, orphaned) reads through its own badge, not "resting". And
// only when genuinely calm — any loud signal (the agent's own or an outside
// mark) supersedes the resting state. Drives the calm "Idle" presentation in
// the list and header.
//
// Unlike a loud outside mark, the idle mark is deliberately NOT subject to
// activity-staleness: it is the agent's own lifecycle self-report, retracted
// event-driven by the `working` hook (monitor `apply_hook` clears IDLE_KEY when
// a prompt is submitted), not by `last_activity_at` advancing. Comparing it
// against activity would misfire two ways: (1) the very turn-ending output that
// triggers the `waiting`/`idle` hook also changes the pane, and the monitor's
// pane-hash touch bumps `last_activity_at` a millisecond *after* the mark's
// `set_at` — so the mark would be born stale and a finished turn would never read
// "Idle"; and (2) sub-agent or shell pane activity under an idle mark is, by
// design, still "resting" (see monitor `apply_hook`), not a reason to retract it.
const LIVE_STATUSES = new Set(['running']);

// Whether the session still has a live agent pane to type into — the gate for
// the conversation composer. `POST /sessions/{id}/send` 409s once the terminal
// is gone (orphaned / done / error / archived), so the composer only shows while
// the agent is running. Reuses LIVE_STATUSES: the same "the agent is here" notion
// as the idle mark.
export function canSend(s: Session): boolean {
  return LIVE_STATUSES.has(s.status);
}

export function idleTag(s: Session): Tag | null {
  if (!LIVE_STATUSES.has(s.status)) return null;
  if (loudTags(s).length > 0) return null;
  return (s.branch.tags ?? []).find((t) => t.key === IDLE_KEY) ?? null;
}

// Compact conversation-state line for the detail header (#5): a derived
// STATE, not verbatim agent chatter. Drives the line that replaces the old
// "Waiting for input" slab. Returns a glyph + short label; glyphs are plain
// unicode (offline-safe, no icon dependency).
export interface ConvState {
  glyph: string; // ● / ▶ / ✓ / ◦ — BMP geometric chars only (emoji like the
  // hourglass render as tofu in the system sans/mono stacks)
  label: string; // e.g. "Blocked — needs input"
  tone: 'block' | 'attn' | 'ok' | 'info' | 'muted'; // which token family colors it
}

export function conversationState(s: Session): ConvState {
  // Lifecycle first for the unambiguous mechanical states.
  if (s.status === 'archived') return { glyph: '◦', label: 'Archived', tone: 'muted' };
  if (s.status === 'orphaned') return { glyph: '◦', label: 'Orphaned — detached', tone: 'muted' };
  if (s.status === 'error') return { glyph: '◦', label: 'Error', tone: 'muted' };

  // Then the resolved attention signal (the loudest live loud tag — the agent's
  // own, or an outside mark).
  const level = effectiveAttention(s).level;
  if (level === 'blocked') return { glyph: '●', label: 'Blocked', tone: 'block' };
  if (level === 'attention') return { glyph: '●', label: 'Needs attention', tone: 'attn' };

  // Calm: an explicit `idle` mark means the agent is resting — surface it even
  // while the lifecycle is still `running` between turns (a finished turn leaves
  // the session running but quiet). Working gets a calm green, resting a calm
  // cyan — quiet hues well below the loud amber/red, which stays the sole signal
  // that something needs a human.
  if (idleTag(s)) return { glyph: '✓', label: 'Idle', tone: 'info' };
  if (s.status === 'running') return { glyph: '▶', label: 'Working', tone: 'ok' };
  return { glyph: '✓', label: 'Idle', tone: 'info' };
}

// Map ConvState.tone to a text-color token. Pages apply this; keeps motion/
// color tokens out of the deriver. `ok`/`info` give the calm states a quiet
// hue (working = green, resting = cyan) so the line reads as alive, not gray —
// still well below the loud amber/red of a raised signal.
export const TONE_TEXT: Record<ConvState['tone'], string> = {
  block: 'text-block',
  attn: 'text-attn',
  ok: 'text-ok',
  info: 'text-info',
  muted: 'text-muted',
};

// A small per-row status dot for the fleet list — a calm splash of scannable
// color so a long fleet reads at a glance instead of as a wall of gray. The hue
// follows the resolved state: blocked/attention reuse the loud axis (red/amber)
// so the dot agrees with the row's signal chip; a resting agent is cyan; a
// live, calm agent is green; anything terminal/detached (done, orphaned, error,
// archived) recedes to faint. The dot is a quiet hairline tone, never the loud
// chip fill, so it adds rhythm without competing with a real raised signal.
export function lifecycleDot(s: Session): string {
  const level = effectiveAttention(s).level;
  if (level === 'blocked') return 'bg-block-line';
  if (level === 'attention') return 'bg-attn-line';
  if (idleTag(s)) return 'bg-info-line';
  if (s.status === 'running') return 'bg-ok-line';
  return 'bg-faint/50';
}

// ---------------------------------------------------------------------------
// Lifecycle actions — which verbs apply to a session, in one place
// ---------------------------------------------------------------------------

export type LifecycleVerb = 'adopt' | 'recover' | 'archive' | 'remove';

export interface LifecycleAction {
  verb: LifecycleVerb;
  /** Imperative label, as it appears on the button or menu item. */
  label: string;
  /** Present participle shown while the action is in flight. */
  busyLabel: string;
  /** What it does, for the menu item's second line and the button's tooltip. */
  hint: string;
  /** Destructive — rendered in the block (danger) tone, last. */
  danger?: boolean;
}

const ADOPT: LifecycleAction = {
  verb: 'adopt',
  label: 'Adopt',
  busyLabel: 'Adopting…',
  hint: 'Recreate the terminal and resume the agent',
};
const RECOVER: LifecycleAction = {
  verb: 'recover',
  label: 'Recover',
  busyLabel: 'Recovering…',
  hint: 'Rebuild the worktree and resume the agent',
};
const ARCHIVE: LifecycleAction = {
  verb: 'archive',
  label: 'Archive',
  busyLabel: 'Archiving…',
  hint: 'Tear down the terminal and worktree, keep the branch',
};
const REMOVE: LifecycleAction = {
  verb: 'remove',
  label: 'Remove',
  busyLabel: 'Removing…',
  hint: 'Delete the session, its worktree and terminal',
  danger: true,
};

// The one action that gets a session unstuck, when it is stuck: an orphaned
// session (terminal gone, worktree intact) is adopted; an archived one (torn
// down, branch kept) is recovered. Surfaced next to the status badge that
// announces the state, so the cure sits with the diagnosis rather than behind a
// menu. Null for every healthy session — there is nothing to fix.
export function remedyAction(s: Session): LifecycleAction | null {
  if (s.status === 'orphaned') return ADOPT;
  if (s.status === 'archived') return RECOVER;
  return null;
}

// Every lifecycle action available on a session, in menu order: its remedy (if
// stuck), then archive (unless it already is), then the destructive remove.
export function lifecycleActions(s: Session): LifecycleAction[] {
  const actions: LifecycleAction[] = [];
  const remedy = remedyAction(s);
  if (remedy) actions.push(remedy);
  if (s.status !== 'archived') actions.push(ARCHIVE);
  actions.push(REMOVE);
  return actions;
}
