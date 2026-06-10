import type { Session, Tag } from '../types';

export type Attention = 'ok' | 'attention' | 'blocked';

// The well-known LOUD tag keys, both on the attention | blocked ladder. Every
// other key is a quiet, free-form pill. Mirrors weaver-core's tag registry.
export const ATTENTION_KEY = 'attention';
export const TRIAGE_KEY = 'triage';
const LOUD_KEYS = new Set<string>([ATTENTION_KEY, TRIAGE_KEY]);

// Severity order for the loud ladder. Absence (no tag) is `ok`, the calm floor.
const SEVERITY: Record<Attention, number> = { ok: 0, attention: 1, blocked: 2 };

// One branch tag by key, or undefined when absent (the calm state).
function tagOf(s: Session, key: string): Tag | undefined {
  return s.branch.tags?.find((t) => t.key === key);
}

// Normalize a stored loud value to the ladder; anything unexpected is `ok`.
function normalize(value: string | undefined): Attention {
  return value === 'attention' || value === 'blocked' ? value : 'ok';
}

// Agent-declared attention, read off the `attention` tag. Archived sessions are
// forced quiet (the agent is gone); an absent tag is the calm `ok` floor.
// Mirrors the backend; keeps stale/archived rows from shouting.
export function levelOf(s: Session): Attention {
  if (s.status === 'archived') return 'ok';
  return normalize(tagOf(s, ATTENTION_KEY)?.value);
}

// The current-state message (Branch.description). Suppressed for archived
// sessions so torn-down workstreams don't show stale chatter.
export function messageOf(s: Session): string {
  if (s.status === 'archived') return '';
  return s.branch.description ?? '';
}

// The overlooker's triage mark, read off the `triage` tag. '' means unmarked
// (no badge). Distinct from levelOf(): that is the agent's own attention; this
// is an outside assessment stamped by an overlooker. Archived sessions show
// nothing.
export function triageOf(s: Session): '' | Attention {
  if (s.status === 'archived') return '';
  const t = tagOf(s, TRIAGE_KEY);
  return t ? normalize(t.value) : '';
}

// Whether a tag predates the session's latest activity — the session has "moved
// on" since the tag was set, so a triage mark may no longer hold. The badge
// renders this faded with a stale hint. No tag, or no activity timestamp, is
// never stale.
export function tagStale(tag: Tag | undefined, lastActivityAt: string): boolean {
  if (!tag || !tag.set_at || !lastActivityAt) return false;
  return lastActivityAt > tag.set_at;
}

// Who raised the resolved attention signal: the agent's own self-report, or an
// overlooker's outside assessment (triage). The pages render the agent's signal
// as the plain loud badge and an overlooker's with the ⊙ "watched" treatment.
export interface EffectiveAttention {
  level: Attention;
  /** Which axis is loudest: 'agent' (its own `attention`) or 'triage' (an
   *  overlooker). 'none' when calm. */
  raisedBy: 'none' | 'agent' | 'triage';
  /** The `set_by` of the loudest tag (the overlooker name, or 'agent'). */
  by: string;
  /** One-line reason from the loudest tag. */
  note: string;
  /** True when a triage mark is the loudest signal but the session has moved on
   *  since it was set, so it should fade. */
  stale: boolean;
}

// The single resolved attention signal the dashboard renders: the louder of the
// agent's `attention` tag and the *non-stale* `triage` tag. The agent saying
// calm while an overlooker says attention surfaces as "needs attention (raised
// by <overlooker>)". A stale triage mark is ignored for the resolved level (the
// session has moved on) but still flagged when it is the only signal, so an
// hour-old "looks stuck" fades rather than lies.
export function effectiveAttention(s: Session): EffectiveAttention {
  if (s.status === 'archived') {
    return { level: 'ok', raisedBy: 'none', by: '', note: '', stale: false };
  }

  const agentTag = tagOf(s, ATTENTION_KEY);
  const triageTag = tagOf(s, TRIAGE_KEY);
  const agentLevel = normalize(agentTag?.value);
  const triageLevel = normalize(triageTag?.value);
  const triageIsStale = tagStale(triageTag, s.last_activity_at);

  // A stale triage mark doesn't drive the resolved level; only a live one does.
  const liveTriage = triageTag && !triageIsStale ? triageLevel : 'ok';

  // The louder axis wins; on a tie the agent's own report takes precedence
  // (its self-report is the primary signal).
  if (SEVERITY[liveTriage] > SEVERITY[agentLevel]) {
    return {
      level: liveTriage,
      raisedBy: 'triage',
      by: triageTag!.set_by || 'overlooker',
      note: triageTag!.note,
      stale: false,
    };
  }
  if (agentLevel !== 'ok') {
    return {
      level: agentLevel,
      raisedBy: 'agent',
      by: agentTag?.set_by || 'agent',
      note: agentTag?.note ?? messageOf(s),
      stale: false,
    };
  }
  // The agent is calm. Surface a stale triage mark, faded, as the lone signal so
  // it stays visible (with attribution) without claiming the session is live.
  if (triageTag) {
    return {
      level: triageLevel,
      raisedBy: 'triage',
      by: triageTag.set_by || 'overlooker',
      note: triageTag.note,
      stale: triageIsStale,
    };
  }
  return { level: 'ok', raisedBy: 'none', by: '', note: '', stale: false };
}

// The session's quiet tags: every tag whose key is not loud, for the pill row.
// These are free-form, deletable annotations (priority, needs-rebase, …).
// Archived sessions show none.
export function quietTags(s: Session): Tag[] {
  if (s.status === 'archived') return [];
  return (s.branch.tags ?? []).filter((t) => !LOUD_KEYS.has(t.key));
}

// Compact conversation-state line for the detail header (#5): a derived
// STATE, not verbatim agent chatter. Drives the line that replaces the old
// "Waiting for input" slab. Returns a glyph + short label; glyphs are plain
// unicode (offline-safe, no icon dependency).
export interface ConvState {
  glyph: string;   // ● / ▶ / ✓ / ◦ — BMP geometric chars only (emoji like the
                   // hourglass render as tofu in the system sans/mono stacks)
  label: string;   // e.g. "Blocked — needs input"
  tone: 'block' | 'attn' | 'muted'; // which token family to color it with
}

export function conversationState(s: Session): ConvState {
  // Lifecycle first for the unambiguous mechanical states.
  if (s.status === 'archived') return { glyph: '◦', label: 'Archived', tone: 'muted' };
  if (s.status === 'orphaned') return { glyph: '◦', label: 'Orphaned — detached', tone: 'muted' };
  if (s.status === 'error') return { glyph: '◦', label: 'Error', tone: 'muted' };

  // Then the resolved attention signal (agent's own, or a non-stale overlooker
  // mark — whichever is louder).
  const level = effectiveAttention(s).level;
  if (level === 'blocked') return { glyph: '●', label: 'Blocked', tone: 'block' };
  if (level === 'attention') return { glyph: '●', label: 'Needs attention', tone: 'attn' };

  // Otherwise infer working vs idle from lifecycle. "Working"/"Idle" stay
  // neutral so amber/red remains the sole loud signal.
  if (s.status === 'running' || s.status === 'launching') return { glyph: '▶', label: 'Working', tone: 'muted' };
  return { glyph: '✓', label: 'Idle', tone: 'muted' };
}

// Map ConvState.tone to a text-color token. Pages apply this; keeps motion/
// color tokens out of the deriver.
export const TONE_TEXT: Record<ConvState['tone'], string> = {
  block: 'text-block',
  attn: 'text-attn',
  muted: 'text-muted',
};
