import type { Watch, WatchAction, WatchRun, WatchTrigger, WatchScope } from '../types';

// A watch stores its `trigger`, `scope`, and `params` — and a run its `actions`
// — as JSON columns, so the API declares them as free-form values and the
// generated types say `unknown`. These four are the only place the browser
// asserts a shape for them.
export const triggerOf = (watch: Pick<Watch, 'trigger'>): WatchTrigger =>
  (watch.trigger ?? {}) as WatchTrigger;
export const scopeOf = (watch: Pick<Watch, 'scope'>): WatchScope =>
  (watch.scope ?? {}) as WatchScope;
export const paramsOf = (watch: Pick<Watch, 'params'>): Record<string, unknown> =>
  (watch.params ?? {}) as Record<string, unknown>;
export const actionsOf = (run: Pick<WatchRun, 'actions'>): WatchAction[] =>
  Array.isArray(run.actions) ? (run.actions as WatchAction[]) : [];

// The intervention ladder, calm → loud (mirrors weaver-core's CAPABILITIES).
// `observe` is implicit — always granted — so the create/edit forms only offer
// the explicit grants below it.
export const CAPABILITIES = [
  'observe',
  'judge',
  'mark',
  'escalate',
  'nudge',
  'interrupt',
  'launch',
] as const;
export const GRANTABLE_CAPABILITIES = [
  'judge',
  'mark',
  'escalate',
  'nudge',
  'interrupt',
  'launch',
] as const;

// Final path segment of a repo root, for a short chip label.
export function repoLabel(path: string): string {
  return path.replace(/\/+$/, '').split('/').pop() || path;
}

// Assemble the capability set a create/edit form sends: the implicit `observe`
// plus the explicitly-ticked grants, in ladder order. Both views feed it the
// same {grant → bool} map so they can't drift on the observe-implicit rule.
export function capabilitiesFrom(ticked: Record<string, boolean>): string[] {
  return ['observe', ...GRANTABLE_CAPABILITIES.filter((c) => ticked[c])];
}

// A one-line, human-readable summary of a trigger — what wakes a round.
// e.g. "cron 0 * * * *", "every 30m", "on pr.merged, pr.opened",
// "on session.attention=blocked". An empty/unset trigger reads as "manual"
// (only fires on Run now). A trigger may carry both a schedule and events.
export function triggerSummary(t: WatchTrigger | undefined | null): string {
  if (!t) return 'manual';
  const parts: string[] = [];
  if (t.cron) parts.push(`cron ${t.cron}`);
  if (t.every) parts.push(`every ${t.every}`);
  // The subscription set: the `on` list plus the legacy single `event`.
  const events = [...(t.on ?? [])];
  if (t.event) events.push(t.level ? `${t.event}=${t.level}` : t.event);
  if (events.length) parts.push(`on ${events.join(', ')}`);
  return parts.length ? parts.join(' · ') : 'manual';
}

// A one-line summary of the fleet scope a round surveys.
// e.g. "attention ≠ ok", "attention = blocked", or "whole fleet".
export function scopeSummary(s: WatchScope | undefined | null): string {
  if (!s || !s.attention) return 'whole fleet';
  const a = s.attention;
  return a.startsWith('!') ? `attention ≠ ${a.slice(1)}` : `attention = ${a}`;
}

// The judgement prompt a stock program runs, pulled out of `params`.
export function promptOf(o: Pick<Watch, 'params'>): string {
  const p = paramsOf(o).prompt;
  return typeof p === 'string' ? p : '';
}
