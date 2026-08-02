import { computed, ref } from 'vue';
import { getSessionLayout, listRuns, listSessionSummaries } from '../api';
import { openTopic, type TopicHandle } from './eventStream';
import type { AutomationRun, SessionLayout, SessionSummary } from '../types';

// One shared snapshot of the fleet. The session list, the status bar, and the
// detail-page tab title all read from here instead of each polling the fleet on
// their own. This store intentionally holds SessionSummary rows, not full
// Session resources: opening a page or row disclosure fetches its detail on
// demand, so the three-second poll never transfers goals, launch snapshots, MCP
// policy, runtime identifiers, or other detail-only fields.
//
// This is the thin-client pattern the rest of loom follows (docs/loom-ui.md):
// the view is a projection of REST state, never a separate browser-local truth.

const activeSessions = ref<SessionSummary[]>([]);
const archivedSessions = ref<SessionSummary[]>([]);
const sessions = computed(() => [...activeSessions.value, ...archivedSessions.value]);
const runs = ref<AutomationRun[]>([]);
const layout = ref<SessionLayout | null>(null);
const resourceErrors = ref<Partial<Record<'sessions' | 'history' | 'runs' | 'layout', string>>>({});
// Last fetch reached the server? Drives the status bar's online dot; the cached
// counts dim rather than vanish while the server is briefly unreachable.
const online = ref(true);
// The mailbox cursor is view state, but the persistent status line also needs
// to describe the row under that cursor. Session detail routes use their own
// route id; this value is consulted only while the Sessions route is active.
const focusedSessionId = ref('');

let inflight: Promise<void> | null = null;
let refreshRequested = false;

// Pull the compact active fleet only. Archived history is both much larger and
// operationally cold, so `loadHistory` fetches it when that view is opened
// instead of transferring it on every three-second tick. Concurrent callers
// coalesce onto one in-flight loop. A request arriving while a snapshot is
// loading marks the loop dirty and guarantees one trailing fetch.
async function refreshActive(): Promise<void> {
  refreshRequested = true;
  if (inflight) return inflight;
  inflight = (async () => {
    while (refreshRequested) {
      refreshRequested = false;
      const results = await Promise.allSettled([
        listSessionSummaries({ automation: true }),
        listRuns(),
        getSessionLayout(),
      ]);
      const resources = ['sessions', 'runs', 'layout'] as const;
      const nextErrors = { ...resourceErrors.value };
      results.forEach((result, index) => {
        const resource = resources[index];
        if (result.status === 'rejected') {
          nextErrors[resource] = (result.reason as Error).message;
          return;
        }
        delete nextErrors[resource];
        if (resource === 'sessions') activeSessions.value = result.value as SessionSummary[];
        if (resource === 'runs') runs.value = result.value as AutomationRun[];
        if (resource === 'layout') layout.value = result.value as SessionLayout;
      });
      resourceErrors.value = nextErrors;
      // One auxiliary projection can fail without declaring the server offline.
      online.value = results.some((result) => result.status === 'fulfilled');
    }
  })().finally(() => {
    inflight = null;
    // Cover the promise-resolution microtask gap between the loop's final
    // condition check and this cleanup.
    if (refreshRequested) void refreshActive();
  });
  return inflight;
}

let historyInflight: Promise<void> | null = null;
let historyLoaded = false;
async function loadHistory(): Promise<void> {
  if (historyInflight) return historyInflight;
  historyInflight = listSessionSummaries({
    archived: true,
    archivedOnly: true,
    automation: true,
  })
    .then((history) => {
      archivedSessions.value = history;
      historyLoaded = true;
      const errors = { ...resourceErrors.value };
      delete errors.history;
      resourceErrors.value = errors;
    })
    .catch((cause) => {
      resourceErrors.value = {
        ...resourceErrors.value,
        history: (cause as Error).message,
      };
      throw cause;
    })
    .finally(() => {
      historyInflight = null;
    });
  return historyInflight;
}

// Explicit mutations refresh any history snapshot the operator has disclosed;
// the background timer below intentionally stays on the active projection.
async function refresh(): Promise<void> {
  await refreshActive();
  if (historyLoaded) await loadHistory();
}

function sessionById(id: string): SessionSummary | undefined {
  return sessions.value.find((s) => s.id === id);
}

function focusSession(id: string): void {
  focusedSessionId.value = id;
}

// One fleet poll for the whole app, started from the shell (App.vue) once the
// caller is authenticated and stopped on sign-out. Guarded so a double-call
// (HMR, a re-mount) can't leave two intervals running.
let timer: number | undefined;
let layoutEvents: TopicHandle | undefined;
const POLL_MS = 3000;

function startFleetPoll(): void {
  if (timer !== undefined) return;
  refreshActive();
  timer = window.setInterval(refreshActive, POLL_MS);
  layoutEvents = openTopic('layout');
  layoutEvents.on('session_layout', () => void refreshActive());
}

function stopFleetPoll(): void {
  if (timer === undefined) return;
  clearInterval(timer);
  timer = undefined;
  layoutEvents?.close();
  layoutEvents = undefined;
}

export function useFleet() {
  return {
    sessions,
    runs,
    layout,
    resourceErrors,
    online,
    focusedSessionId,
    focusSession,
    refresh,
    loadHistory,
    sessionById,
    startFleetPoll,
    stopFleetPoll,
  };
}
