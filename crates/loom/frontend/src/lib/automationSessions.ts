import type { AutomationRun, Session } from '../types';
import { effectiveAttention } from './sessionState';

const ACTIVE = new Set(['created', 'running']);
const HISTORY = new Set(['done', 'archived']);

export function isAutomationHistory(session: Session): boolean {
  return HISTORY.has(session.status);
}

/** Lifecycle failures are operational exceptions even when no agent/watch has
 *  had a chance to stamp a loud tag. Unknown live states fail open into the
 *  intervention queue rather than disappearing. */
export function needsAutomationIntervention(session: Session): boolean {
  if (isAutomationHistory(session)) return false;
  if (session.status === 'error' || session.status === 'orphaned') return true;
  if (!ACTIVE.has(session.status)) return true;
  return effectiveAttention(session).level !== 'ok';
}

/** Blocked first, then mechanical failures, then attention, then calm work. */
export function automationPriority(session: Session): number {
  const level = effectiveAttention(session).level;
  if (level === 'blocked') return 0;
  if (session.status === 'error' || session.status === 'orphaned') return 1;
  if (level === 'attention') return 2;
  return 3;
}

export function byAutomationPriority(a: Session, b: Session): number {
  return (
    automationPriority(a) - automationPriority(b) ||
    (b.last_activity_at || '').localeCompare(a.last_activity_at || '')
  );
}

/** Runs with sessions render through the richer Session row. Only reservations
 *  whose preallocated session id is absent need their own operational row. */
export function unmatchedAutomationRuns(
  runs: AutomationRun[],
  sessions: Session[],
): AutomationRun[] {
  const sessionIds = new Set(sessions.map((session) => session.id));
  return runs.filter((run) => !sessionIds.has(run.session_id));
}

export function runNeedsIntervention(run: AutomationRun): boolean {
  return run.status === 'failed';
}

export function isAutomationRunHistory(run: AutomationRun): boolean {
  return run.status === 'cancelled' || run.status === 'completed';
}
