import { ref } from 'vue';
import { listRuns, listSessions } from '../api';
import type { AutomationRun, Session } from '../types';

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
// Last fetch reached the server? Drives the status bar's online dot; the cached
// counts dim rather than vanish while the server is briefly unreachable.
const online = ref(true);

let inflight: Promise<void> | null = null;

// Pull the whole fleet, archived and automation-class sessions included — the
// superset the Workspace/Automation panes and archive disclosures need (the
// status bar and each pane project their own subset). Concurrent callers
// coalesce onto the one in-flight request.
async function refresh(): Promise<void> {
  if (inflight) return inflight;
  inflight = (async () => {
    try {
      const [nextSessions, nextRuns] = await Promise.all([
        listSessions({ archived: true, automation: true }),
        listRuns(),
      ]);
      sessions.value = nextSessions;
      runs.value = nextRuns;
      online.value = true;
    } catch {
      // Keep the last good snapshot; the status bar's offline dot says why.
      online.value = false;
    } finally {
      inflight = null;
    }
  })();
  return inflight;
}

function sessionById(id: string): Session | undefined {
  return sessions.value.find((s) => s.id === id);
}

// One fleet poll for the whole app, started from the shell (App.vue) once the
// caller is authenticated and stopped on sign-out. Guarded so a double-call
// (HMR, a re-mount) can't leave two intervals running.
let timer: number | undefined;
const POLL_MS = 3000;

function startFleetPoll(): void {
  if (timer !== undefined) return;
  refresh();
  timer = window.setInterval(refresh, POLL_MS);
}

function stopFleetPoll(): void {
  if (timer === undefined) return;
  clearInterval(timer);
  timer = undefined;
}

export function useFleet() {
  return { sessions, runs, online, refresh, sessionById, startFleetPoll, stopFleetPoll };
}
