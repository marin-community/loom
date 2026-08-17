import type { AutomationRun } from '../types';
import type { UnmatchedRunProjection } from './automationSessions';

export function unmatchedRunTitle(
  run: Pick<AutomationRun, 'status'>,
  projection: UnmatchedRunProjection,
): string {
  if (projection === 'intervention') {
    return run.status === 'waiting' ? 'Launch blocked' : `Launch ${run.status}`;
  }
  if (projection === 'history') return `Run ${run.status}`;
  return `Provisioning · ${run.status}`;
}
