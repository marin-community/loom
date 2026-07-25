// Privacy-safe workbench navigation timing. These metrics intentionally carry
// only a session id, navigation source, and duration — never a title, goal,
// prompt, status message, artifact name, or conversation content.
//
// Performance entries make the timings available to browser tooling. The
// `loom:ui-metric` event is the narrow collection seam for a deployment that
// wants to aggregate them later without coupling route components to telemetry.

export interface WorkbenchMetric {
  name: 'session_open' | 'session_backtrack';
  session_id: string;
  source: 'list' | 'direct';
  duration_ms?: number;
}

const LIST_TO_SESSION = 'weaver:list-to-session';
const SESSION_BACKTRACK = 'weaver:session-backtrack';
const LIST_OPEN_MARK = 'weaver:list-open-start';

let pendingListOpen: { sessionId: string; startedAt: number } | null = null;
let activeSession: { sessionId: string; openedAt: number; source: 'list' | 'direct' } | null = null;

function emit(metric: WorkbenchMetric): void {
  window.dispatchEvent(new CustomEvent<WorkbenchMetric>('loom:ui-metric', { detail: metric }));
}

/** Called by the real session link, so pointer and keyboard opens share timing. */
export function beginSessionOpen(sessionId: string): void {
  performance.clearMarks(LIST_OPEN_MARK);
  performance.mark(LIST_OPEN_MARK);
  pendingListOpen = { sessionId, startedAt: performance.now() };
}

/** Called when the detail route has painted its cached or freshly loaded shell. */
export function completeSessionOpen(sessionId: string): void {
  const fromList = pendingListOpen?.sessionId === sessionId;
  const source = fromList ? 'list' : 'direct';
  let durationMs: number | undefined;

  if (fromList && pendingListOpen) {
    durationMs = Math.max(0, performance.now() - pendingListOpen.startedAt);
    performance.clearMeasures(LIST_TO_SESSION);
    performance.measure(LIST_TO_SESSION, LIST_OPEN_MARK);
  }

  activeSession = { sessionId, openedAt: performance.now(), source };
  pendingListOpen = null;
  performance.clearMarks(LIST_OPEN_MARK);
  emit({ name: 'session_open', session_id: sessionId, source, duration_ms: durationMs });
}

/** Records the return path; consumers can choose their own wrong-open threshold. */
export function recordSessionListReturn(): void {
  if (!activeSession) return;
  const durationMs = Math.max(0, performance.now() - activeSession.openedAt);
  performance.clearMeasures(SESSION_BACKTRACK);
  performance.measure(SESSION_BACKTRACK, {
    start: activeSession.openedAt,
    duration: durationMs,
  });
  emit({
    name: 'session_backtrack',
    session_id: activeSession.sessionId,
    source: activeSession.source,
    duration_ms: durationMs,
  });
  activeSession = null;
}
