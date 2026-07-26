import { ref } from 'vue';
import { getSessionLayout, listRuns, listSessions } from '../api';
import type { AutomationRun, Session, SessionLayout } from '../types';

// One shared snapshot of the fleet. The session list, the status bar, and the
// detail page all read from here instead of each polling `/api/sessions` on
// their own. The payoff is snappiness: the data is fetched once per tick (not
// three overlapping times), it's already present the instant any view mounts —
// so returning to the fleet never flashes an empty state or re-runs an entrance
// animation — and the detail page can paint from the cached row immediately
// rather than showing "Loading…" while it refetches what the list already has.
//
// This is the thin-client pattern the rest of loom follows (docs/loom-ui.md):
// the view is a projection of REST state, never a separate browser-local truth.

const sessions = ref<Session[]>([]);
const runs = ref<AutomationRun[]>([]);
const layout = ref<SessionLayout | null>(null);
const resourceErrors = ref<Partial<Record<'sessions' | 'runs' | 'layout', string>>>({});
// Last fetch reached the server? Drives the status bar's online dot; the cached
// counts dim rather than vanish while the server is briefly unreachable.
const online = ref(true);

let inflight: Promise<void> | null = null;
let refreshRequested = false;

// Pull the whole inventory, archived and automation-class sessions included —
// the superset the Spaces, Attention, and explicit History views need. Concurrent
// callers coalesce onto one in-flight loop. A request arriving while a
// snapshot is loading marks the loop dirty and guarantees one trailing fetch.
async function refresh(): Promise<void> {
  refreshRequested = true;
  if (inflight) return inflight;
  inflight = (async () => {
    while (refreshRequested) {
      refreshRequested = false;
      const results = await Promise.allSettled([
        listSessions({ archived: true, automation: true }),
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
        if (resource === 'sessions') sessions.value = result.value as Session[];
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
    if (refreshRequested) void refresh();
  });
  return inflight;
}

function sessionById(id: string): Session | undefined {
  return sessions.value.find((s) => s.id === id);
}

// One fleet poll for the whole app, started from the shell (App.vue) once the
// caller is authenticated and stopped on sign-out. Guarded so a double-call
// (HMR, a re-mount) can't leave two intervals running.
let timer: number | undefined;
let layoutEvents: EventSource | undefined;
const POLL_MS = 3000;

function startFleetPoll(): void {
  if (timer !== undefined) return;
  refresh();
  timer = window.setInterval(refresh, POLL_MS);
  layoutEvents = new EventSource('/api/session-layout/events');
  layoutEvents.addEventListener('session_layout', () => void refresh());
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
    refresh,
    sessionById,
    startFleetPoll,
    stopFleetPoll,
  };
}
