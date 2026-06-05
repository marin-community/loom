import type { Session } from '../types';

export type Attention = 'ok' | 'attention' | 'blocked';

// Agent-declared attention, normalized. Archived sessions are forced quiet
// (the agent is gone), and unset attention defaults to 'ok'. Mirrors the
// backend; keeps stale/archived rows from shouting.
export function levelOf(s: Session): Attention {
  if (s.status === 'archived') return 'ok';
  const a = s.branch.attention;
  return a === 'attention' || a === 'blocked' ? a : 'ok';
}

// The current-state message (Branch.description). Suppressed for archived
// sessions so torn-down workstreams don't show stale chatter.
export function messageOf(s: Session): string {
  if (s.status === 'archived') return '';
  return s.branch.description ?? '';
}

// Compact conversation-state line for the detail header (#5): a derived
// STATE, not verbatim agent chatter. Drives the line that replaces the old
// "Waiting for input" slab. Returns a glyph + short label; glyphs are plain
// unicode (offline-safe, no icon dependency).
export interface ConvState {
  glyph: string;   // ⏳ / ▶ / ✓ / ◦
  label: string;   // e.g. "Blocked — needs input"
  tone: 'block' | 'attn' | 'muted'; // which token family to color it with
}

export function conversationState(s: Session): ConvState {
  // Lifecycle first for the unambiguous mechanical states.
  if (s.status === 'archived') return { glyph: '◦', label: 'Archived', tone: 'muted' };
  if (s.status === 'orphaned') return { glyph: '◦', label: 'Orphaned — detached', tone: 'muted' };
  if (s.status === 'error') return { glyph: '◦', label: 'Error', tone: 'muted' };

  // Then the agent-declared attention axis.
  const level = levelOf(s);
  if (level === 'blocked') return { glyph: '⏳', label: 'Blocked', tone: 'block' };
  if (level === 'attention') return { glyph: '⏳', label: 'Needs attention', tone: 'attn' };

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
