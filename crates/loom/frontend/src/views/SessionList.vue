<script setup lang="ts">
import { ref, computed, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { Session } from '../types';
import StatusBadge from '../components/StatusBadge.vue';
import SignalChip from '../components/SignalChip.vue';
import IdleChip from '../components/IdleChip.vue';
import TagPill from '../components/TagPill.vue';
import GithubStatus from '../components/GithubStatus.vue';
import NewSessionDrawer from '../components/NewSessionDrawer.vue';
import SessionRowActions from '../components/SessionRowActions.vue';
import SessionRemedyButton from '../components/SessionRemedyButton.vue';
import AgentUsage from '../components/AgentUsage.vue';
import AutomationSessions from '../components/AutomationSessions.vue';
import { timeAgo } from '../lib/time';
import {
  isAutomationHistory,
  isAutomationRunHistory,
  needsAutomationIntervention,
  runNeedsIntervention,
  unmatchedAutomationRuns,
} from '../lib/automationSessions';
import {
  effectiveAttention,
  idleTag,
  lifecycleDot,
  messageOf,
  orderKey,
  parkLabel,
  priorityRank,
  quietTags,
  shelved,
  signalChips,
} from '../lib/sessionState';
import { useFleet } from '../lib/sessionsStore';
import { del, setSessionOrder, setSessionPark } from '../api';

// Named so App.vue's <keep-alive :include> matches it — the fleet list stays
// alive across navigation (instant return, no refetch flash, no re-animate).
defineOptions({ name: 'SessionList' });

// The shared fleet snapshot — polled once for the whole app (App.vue), so this
// view paints from cache the instant it mounts (and, kept alive, stays painted
// across navigation — no refetch flash, no re-animate). `refresh()` forces an
// immediate re-pull after a write (create / clear tag).
const { sessions, runs, refresh } = useFleet();

// Attention filter — the dashboard's "which sessions need me?" control. The
// URL query is the source of truth (`/?filter=attention`, the status bar's
// deep-link): the buttons write it via router.replace and the ref follows, so
// the view is shareable and survives reload/back-forward.
type AttentionFilter = 'all' | 'attention' | 'ok';
const route = useRoute();
const router = useRouter();
function filterFromQuery(q: unknown): AttentionFilter {
  return q === 'attention' || q === 'ok' ? q : 'all';
}
const filter = ref<AttentionFilter>(filterFromQuery(route.query.filter));
// The component is reused when only the query changes (status-bar click while
// already on the fleet list), so track it.
watch(
  () => route.query.filter,
  (q) => (filter.value = filterFromQuery(q)),
);
// Button clicks update the query (replace, not push — filter flips shouldn't
// pollute history); the watcher above folds it back into the ref.
function setFilter(f: AttentionFilter) {
  filter.value = f;
  router.replace({ query: { ...route.query, filter: f === 'all' ? undefined : f } });
}

// The Sessions route has two URL-backed surfaces. Workspace is the default and
// remains the human workbench; Automation is a purpose-built operational view.
// Kept-alive route changes (including back/forward) flow through this computed
// value without remounting the list.
type SessionPane = 'workspace' | 'automation';
const pane = computed<SessionPane>(() =>
  route.query.view === 'automation' ? 'automation' : 'workspace',
);
const workspaceSessions = computed(() =>
  sessions.value.filter((session) => session.class !== 'automation'),
);
const automationSessions = computed(() =>
  sessions.value.filter((session) => session.class === 'automation'),
);
const unmatchedRuns = computed(() => unmatchedAutomationRuns(runs.value, automationSessions.value));
const liveAutomationCount = computed(
  () =>
    automationSessions.value.filter((session) => !isAutomationHistory(session)).length +
    unmatchedRuns.value.filter((run) => !isAutomationRunHistory(run)).length,
);
const automationInterventionCount = computed(
  () =>
    automationSessions.value.filter(needsAutomationIntervention).length +
    unmatchedRuns.value.filter(runNeedsIntervention).length,
);
const historyOpen = computed(() => pane.value === 'automation' && route.query.history === 'true');

function paneQuery(next: SessionPane): Record<string, string | null | undefined> {
  const query = { ...route.query };
  if (next === 'automation') {
    delete query.filter;
    delete query.new;
    query.view = 'automation';
  } else {
    delete query.view;
    delete query.history;
  }
  return query as Record<string, string | null | undefined>;
}

function toggleAutomationHistory() {
  const query = paneQuery('automation');
  if (historyOpen.value) delete query.history;
  else query.history = 'true';
  router.replace({ query });
}

// Archived sessions are torn-down workstreams: kept for reference but clutter
// the live fleet view. Hide them by default; a reveal chip brings them back.
// They still show when there's nothing else to look at (an all-archived fleet),
// so the list never reads as empty while archived rows exist.
const showArchived = ref(false);

// Every existing Workspace calculation reads this interactive-only base.
// Automation has its own component and cannot leak into workspace counts,
// threading, manual ordering, or the Parked shelf.
const filteredBase = computed<Session[]>(() => workspaceSessions.value);

const archivedCount = computed(
  () => filteredBase.value.filter((s) => s.status === 'archived').length,
);

// The archived-aware, filtered set to display (membership only — the tree below
// imposes the order). Archived rows are hidden by default; a reveal chip brings
// them back, and they always show when there's nothing else, so the list never
// reads empty while archived rows exist. Individual attention rows aren't pinned
// to the top — threading keeps related work grouped — but a whole thread that
// contains attention floats up (see treeRows), and attention rows carry their
// loud signal chip, so they stay easy to spot.
const visibleSessions = computed<Session[]>(() => {
  const all = filteredBase.value;
  const live = all.filter((s) => s.status !== 'archived');
  const base = showArchived.value || live.length === 0 ? all : live;
  if (filter.value === 'attention') return base.filter((s) => effectiveAttention(s).level !== 'ok');
  if (filter.value === 'ok') return base.filter((s) => effectiveAttention(s).level === 'ok');
  return base;
});

// Counts reflect the full fleet (NOT the archived-hidden view, but automation-
// aware) so the filter chips read the true picture; effectiveAttention()
// already forces archived → ok and ignores stale watch marks, keeping "needs
// attention" honest.
const counts = computed(() => {
  const c = { all: filteredBase.value.length, attention: 0, ok: 0 };
  for (const s of filteredBase.value) {
    if (effectiveAttention(s).level === 'ok') c.ok += 1;
    else c.attention += 1; // 'attention' and 'blocked' both need a human
  }
  return c;
});

// One flattened tree row: the session, its depth, and the gutter guides drawn to
// its left. `verticals[i]` is true when an ancestor's line keeps running down
// past this row; `isLast` picks the connector (└ vs ├). Depth 0 (top-level)
// draws no gutter, so a flat fleet with no sub-sessions looks exactly as before.
interface TreeRow {
  session: Session;
  depth: number;
  verticals: boolean[];
  isLast: boolean;
}

// Group the visible sessions into threads: each hangs under the session that
// launched it (`parent_id`, a branch id), with top-level sessions under an
// implicit root. A parent always sits directly above its children. Threads stay
// grouped, but a thread containing attention floats to the top: roots and
// siblings sort by their subtree's urgency first (blocked above attention above
// ok), then newest-first within the same urgency — so a blocked/attention child
// can never sink to the bottom of the fleet. A parent that's filtered/archived
// out of the visible set (or was never tracked) drops its orphaned children up.
// The shared tree build. Groups visible sessions into threads, ranks each
// thread by its loudest member, then splits the top-level threads two ways: the
// live list, and the resting "Parked" shelf (long-idle or hand-parked threads
// that need nothing from you). Live threads carry the manual drag order;
// the shelf sorts by most-recent activity. Exposes the ordered live roots so the
// drop math can find a dragged row's new neighbours.
const partitioned = computed(() => {
  const list = visibleSessions.value;
  const present = new Set(list.map((s) => s.branch.id));
  const children = new Map<string, Session[]>();
  const roots: Session[] = [];
  for (const s of list) {
    const pid = s.parent_id;
    if (pid && pid !== s.branch.id && present.has(pid)) {
      const arr = children.get(pid);
      if (arr) arr.push(s);
      else children.set(pid, [s]);
    } else {
      roots.push(s);
    }
  }
  // Render rank for a single session: blocked is louder than attention is louder
  // than the calm default, and a parked row (waiting on an external reviewer —
  // nothing for the user to do) sinks below it. Used to float urgent threads to
  // the top and sink parked ones to the bottom. Uses the resolved signal (agent's
  // own report or a non-stale watch mark) so a triaged thread floats too —
  // matching visibleSessions/counts above.
  const rankOf = (s: Session): number => priorityRank(s);
  // Memoized max render rank across a session's whole subtree (itself + all
  // descendants), so a thread surfaces at the urgency of its loudest member — and
  // a parked parent with live (rank-0) children stays at the calm default rather
  // than sinking the whole thread. The cycle guard mirrors walk()'s: a parent
  // link loop can't spin us forever.
  const rankCache = new Map<string, number>();
  const ranking = new Set<string>();
  const subtreeRank = (s: Session): number => {
    const cached = rankCache.get(s.branch.id);
    if (cached !== undefined) return cached;
    if (ranking.has(s.branch.id)) return rankOf(s); // mid-cycle: just self
    ranking.add(s.branch.id);
    let max = rankOf(s);
    for (const kid of children.get(s.branch.id) ?? []) {
      max = Math.max(max, subtreeRank(kid));
    }
    ranking.delete(s.branch.id);
    rankCache.set(s.branch.id, max);
    return max;
  };
  // Newest-first within a sibling group, matching the dashboard's default feel.
  const byNewest = (a: Session, b: Session) =>
    b.created_at < a.created_at ? -1 : b.created_at > a.created_at ? 1 : 0;
  // Urgent subtree first, then newest-first as the tie-break. When no session
  // carries attention every rank is 0 and this collapses to plain byNewest.
  const byUrgencyThenNewest = (a: Session, b: Session) =>
    subtreeRank(b) - subtreeRank(a) || byNewest(a, b);

  // Split top-level threads. A thread goes to the shelf when its root rests AND
  // nothing in its subtree needs a human (subtreeRank ≤ 0) — a blocked child
  // keeps the whole thread live even under a hand-parked parent.
  const liveRoots: Session[] = [];
  const shelfRoots: Session[] = [];
  for (const r of roots) {
    if (shelved(r) && subtreeRank(r) <= 0) shelfRoots.push(r);
    else liveRoots.push(r);
  }
  // Live order: the manual/auto interleave key (drag places a row exactly; the
  // rest keep urgency-then-newest). Shelf order: most-recently-active first.
  liveRoots.sort(
    (a, b) => orderKey(a, subtreeRank(a)) - orderKey(b, subtreeRank(b)) || byNewest(a, b),
  );
  shelfRoots.sort((a, b) => (b.last_activity_at || '').localeCompare(a.last_activity_at || ''));

  const buildRows = (rootsList: Session[]): TreeRow[] => {
    const rows: TreeRow[] = [];
    const seen = new Set<string>(); // guard against any cycle in the parent links
    const walk = (node: Session, depth: number, verticals: boolean[], isLast: boolean) => {
      if (seen.has(node.branch.id)) return;
      seen.add(node.branch.id);
      rows.push({ session: node, depth, verticals, isLast });
      const kids = [...(children.get(node.branch.id) ?? [])].sort(byUrgencyThenNewest);
      kids.forEach((kid, i) => {
        const last = i === kids.length - 1;
        // The implicit root isn't a drawn column, so a top-level node's children
        // start with no ancestor lines; deeper, append this node's continuation.
        const childVerticals = depth === 0 ? [] : [...verticals, !isLast];
        walk(kid, depth + 1, childVerticals, last);
      });
    };
    rootsList.forEach((r, i) => walk(r, 0, [], i === rootsList.length - 1));
    return rows;
  };

  return {
    liveRows: buildRows(liveRoots),
    shelfRows: buildRows(shelfRoots),
    liveRoots,
    rankOf: (s: Session) => subtreeRank(s),
  };
});

const liveRows = computed(() => partitioned.value.liveRows);
const shelfRows = computed(() => partitioned.value.shelfRows);
// Threads on the shelf (top-level rows only) — the count beside "Parked".
const shelfCount = computed(() => shelfRows.value.filter((r) => r.depth === 0).length);

// ── Drag to reorder · drag to park ──────────────────────────────────────────
// One gesture does both: drag a row within the live list to place it (persists a
// midpoint sort key), or drag it onto the Parked shelf to rest it (and drag a
// resting row back out to keep it live). Only top-level threads drag; children
// ride with their root. HTML5 native DnD so keyboard/`⌘`-click on the row's link
// still work — the grip is the only draggable handle.
const draggingId = ref('');
const dropBeforeId = ref(''); // the live row the dragged thread would land above
const dropAtEnd = ref(false); // …or past the last live row
const overShelf = ref(false); // hovering the shelf drop zone
const parkedOpen = ref(false); // the shelf disclosure

function onDragStart(id: string, e: DragEvent) {
  draggingId.value = id;
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', id); // Firefox needs a payload to drag
  }
}

function onDragEnd() {
  draggingId.value = '';
  dropBeforeId.value = '';
  dropAtEnd.value = false;
  overShelf.value = false;
}

// Hovering a live row: mark whether we'd drop above or below it (pointer vs the
// row's vertical midpoint). Only top-level rows are drop anchors.
function onRowDragOver(rowId: string, e: DragEvent) {
  if (!draggingId.value) return;
  e.preventDefault();
  overShelf.value = false;
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const below = e.clientY > rect.top + rect.height / 2;
  const roots = partitioned.value.liveRoots;
  const idx = roots.findIndex((r) => r.id === rowId);
  if (idx === -1) return;
  const anchor = below ? roots[idx + 1] : roots[idx];
  dropAtEnd.value = below && idx === roots.length - 1;
  dropBeforeId.value = dropAtEnd.value ? '' : (anchor?.id ?? '');
}

function neighbourKey(root: Session | undefined): number | null {
  if (!root) return null;
  return orderKey(root, partitioned.value.rankOf(root));
}

// Commit a live-list drop: place the dragged thread between its new neighbours by
// giving it the midpoint sort key, and — if it came off the shelf — keep it live.
async function commitReorder() {
  const id = draggingId.value;
  if (!id) return;
  const roots = partitioned.value.liveRoots.filter((r) => r.id !== id);
  let above: Session | undefined;
  let below: Session | undefined;
  if (dropAtEnd.value) {
    above = roots[roots.length - 1];
  } else {
    const idx = roots.findIndex((r) => r.id === dropBeforeId.value);
    below = idx === -1 ? roots[0] : roots[idx];
    above = idx <= 0 ? undefined : roots[idx - 1];
  }
  const ka = neighbourKey(above);
  const kb = neighbourKey(below);
  // Midpoint, with a wide gap when dropping past an end so there's room to spare.
  const key =
    ka !== null && kb !== null
      ? (ka + kb) / 2
      : ka !== null
        ? ka + 1e9
        : kb !== null
          ? kb - 1e9
          : 0;
  const dragged = sessions.value.find((s) => s.id === id);
  const wasShelved = dragged ? shelved(dragged) : false;
  // Optimistic: reflect immediately, then persist.
  if (dragged) {
    dragged.sort_order = key;
    if (wasShelved) dragged.park = 'active';
  }
  onDragEnd();
  try {
    if (wasShelved) await setSessionPark(id, 'active');
    await setSessionOrder(id, key);
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}

// Commit a shelf drop: rest the dragged thread on the shelf.
async function commitPark() {
  const id = draggingId.value;
  if (!id) return;
  const dragged = sessions.value.find((s) => s.id === id);
  if (dragged) dragged.park = 'parked';
  parkedOpen.value = true;
  onDragEnd();
  try {
    await setSessionPark(id, 'parked');
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}

// The row ⋯ menu's Park / Keep-live verbs — the accessible, no-drag path.
async function setPark(id: string, state: 'parked' | 'active' | 'auto') {
  const dragged = sessions.value.find((s) => s.id === id);
  if (dragged) dragged.park = state === 'auto' ? null : state;
  if (state === 'parked') parkedOpen.value = true; // reveal where the row landed
  try {
    await setSessionPark(id, state);
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}
const error = ref('');
const MISSING_GITHUB_TOKEN_ERROR = 'No GitHub token configured.';
const tokenConfigWarning = computed(() => error.value.startsWith(MISSING_GITHUB_TOKEN_ERROR));

// The New Session drawer is reflected in the URL (`/?new`), like the attention
// filter above — so it's deep-linkable, the back button closes it, and the tab
// title (composed in App.vue) can read "Weaver - New Session" while it's open.
const showForm = computed(() => pane.value === 'workspace' && route.query.new !== undefined);
function openForm() {
  router.replace({ query: { ...route.query, new: null } });
}
function closeForm() {
  const query = { ...route.query };
  delete query.new;
  router.replace({ query });
}

// A quiet pill's × clears that tag, then refreshes the row. The tag write
// surface is the same DELETE the detail page uses.
const clearingTag = ref('');
async function clearTag(sessionId: string, key: string) {
  clearingTag.value = `${sessionId}:${key}`;
  try {
    await del(`/sessions/${sessionId}/tags/${encodeURIComponent(key)}`);
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    clearingTag.value = '';
  }
}

async function handleCreated() {
  try {
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}
</script>

<template>
  <div class="px-5 py-3">
    <!-- One toolbar line: the view label, the attention filter, the archived
         reveal, and the primary action — no page-hero heading (the rail
         already says where you are; the h1 stays for a11y + tests). -->
    <div class="mb-3 flex min-h-7 flex-wrap items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Sessions</h1>

      <nav
        aria-label="Session surface"
        class="inline-flex overflow-hidden rounded border border-line text-xs"
        data-testid="session-panes"
      >
        <router-link
          :to="{ path: '/', query: paneQuery('workspace') }"
          data-testid="workspace-pane-link"
          :aria-current="pane === 'workspace' ? 'page' : undefined"
          :class="[
            'flex items-center gap-1.5 px-2.5 py-1 font-medium transition-colors',
            pane === 'workspace'
              ? 'bg-accent text-accent-fg'
              : 'bg-input text-muted hover:bg-subtle hover:text-fg',
          ]"
        >
          Workspace
          <span
            class="rounded-full px-1.5 text-2xs leading-4"
            :class="pane === 'workspace' ? 'bg-accent-fg/20' : 'bg-subtle text-faint'"
            :aria-label="`${workspaceSessions.length} workspace sessions`"
          >
            {{ workspaceSessions.length }}
          </span>
        </router-link>
        <router-link
          :to="{ path: '/', query: paneQuery('automation') }"
          data-testid="automation-pane-link"
          :aria-current="pane === 'automation' ? 'page' : undefined"
          :class="[
            'flex items-center gap-1.5 border-l border-line px-2.5 py-1 font-medium transition-colors',
            pane === 'automation'
              ? 'bg-accent text-accent-fg'
              : 'bg-input text-muted hover:bg-subtle hover:text-fg',
          ]"
        >
          Automation
          <span
            class="rounded-full px-1.5 text-2xs leading-4"
            :class="pane === 'automation' ? 'bg-accent-fg/20' : 'bg-subtle text-faint'"
            :aria-label="`${liveAutomationCount} live automation runs`"
          >
            {{ liveAutomationCount }}
          </span>
          <span
            v-if="automationInterventionCount"
            class="rounded bg-block-soft px-1.5 text-2xs text-block ring-1 ring-inset ring-block-line/30"
            data-testid="automation-intervention-badge"
            :aria-label="`${automationInterventionCount} automation runs need intervention`"
          >
            {{ automationInterventionCount }} need intervention
          </span>
        </router-link>
      </nav>

      <!-- Attention filter: jump straight to the sessions that need a human.
           Each segment pairs a label with its count in a small pill so the
           number reads as a count, not a suffix glued to the word. -->
      <div
        v-if="pane === 'workspace' && filteredBase.length"
        class="inline-flex overflow-hidden rounded border border-line text-xs"
      >
        <button
          v-for="opt in ['all', 'attention', 'ok'] as const"
          :key="opt"
          type="button"
          :data-testid="`filter-${opt}`"
          :class="[
            'flex items-center gap-1.5 border-l border-line px-2.5 py-1 font-medium transition-colors first:border-l-0',
            filter === opt
              ? 'bg-accent text-accent-fg'
              : 'bg-input text-muted hover:bg-subtle hover:text-fg',
          ]"
          @click="setFilter(opt)"
        >
          {{ opt === 'all' ? 'All' : opt === 'attention' ? 'Needs attention' : 'OK' }}
          <span
            :class="[
              'rounded-full px-1.5 text-2xs leading-4',
              filter === opt ? 'bg-accent-fg/20 text-accent-fg' : 'bg-subtle text-faint',
            ]"
            >{{ counts[opt] }}</span
          >
        </button>
      </div>

      <!-- Archived live below the fold: a quiet chip reveals/hides them. -->
      <button
        v-if="pane === 'workspace' && archivedCount"
        type="button"
        :aria-pressed="showArchived"
        :class="[
          'rounded border border-line px-2.5 py-1 text-xs text-muted transition-colors',
          showArchived ? 'bg-subtle text-fg' : 'bg-input hover:bg-subtle',
        ]"
        @click="showArchived = !showArchived"
      >
        {{ showArchived ? 'Hide' : 'Show' }} {{ archivedCount }} archived
      </button>

      <!-- Toggles the create form. Closed → primary (accent) call-to-action;
           open → a neutral "Cancel" so it never reads as a second primary
           action competing with the form's own Create button. -->
      <button
        v-if="pane === 'workspace'"
        :class="[
          'ml-auto px-2.5 py-1 text-xs font-medium',
          showForm ? 'btn-secondary' : 'btn-primary',
        ]"
        @click="showForm ? closeForm() : openForm()"
      >
        {{ showForm ? 'Cancel' : 'New session' }}
      </button>
    </div>

    <NewSessionDrawer v-if="showForm" @close="closeForm" @created="handleCreated" />

    <div v-if="error" class="mb-4 text-sm text-block">
      <template v-if="tokenConfigWarning">
        {{ MISSING_GITHUB_TOKEN_ERROR }}
        <RouterLink
          class="text-accent underline"
          :to="{ path: '/settings', query: { tab: 'account' } }"
          >Add your GitHub token</RouterLink
        >
        or configure
        <RouterLink
          class="text-accent underline"
          :to="{ path: '/settings', query: { tab: 'profiles' } }"
          >the selected profile</RouterLink
        >
        with a write-only <code class="font-mono">GH_TOKEN</code> before creating an agent session.
      </template>
      <template v-else>{{ error }}</template>
    </div>

    <div
      v-if="pane === 'workspace' && !filteredBase.length"
      class="rounded-md border border-dashed border-line p-6 text-center"
    >
      <p class="text-sm text-muted">No sessions yet.</p>
      <p class="mt-1 text-xs text-faint">
        Launch one with <strong>New session</strong>, or
        <code>loom session launch "&lt;goal&gt;"</code>.
      </p>
    </div>

    <!--
      One row per session. Left→right: an optional tree gutter threading child
      sessions under their launcher, the dominant title with its loud signal
      chips (attention/triage, each deletable) and quiet tag pills alongside, a
      muted current-state line, a neutral lifecycle pill (shown only for
      off-nominal states — running is the silent default), and the mono branch
      ref pushed far-right. Rows are grouped into threads (built in script), with
      attention-carrying threads floated up so the loud chips surface near the
      top. The row itself stays neutral — no full-tile wash — so threading reads
      cleanly; the chip carries the signal. Stagger via --i.
    -->
    <!-- No `overflow-hidden` on the list: a row's ⋯ menu drops out of its row and
         would be clipped by it. The corners the clip used to round are rounded on
         the first/last row instead. -->
    <!-- Every session is resting — the live list is empty but the fleet isn't.
         Point at the shelf below rather than reading as "no sessions". -->
    <p
      v-if="pane === 'workspace' && filteredBase.length && !liveRows.length"
      class="rounded-md border border-dashed border-line px-4 py-3 text-center font-serif text-[13px] italic text-muted"
    >
      All sessions are resting on the shelf below.
    </p>

    <ul
      v-if="pane === 'workspace' && liveRows.length"
      data-testid="session-list"
      class="fade-in rounded-md border border-line bg-surface"
      @dragover.prevent
      @drop.prevent="commitReorder"
    >
      <li
        v-for="{ session: s, depth, verticals, isLast } in liveRows"
        :key="s.id"
        data-testid="session-card"
        :data-session-id="s.id"
        :data-depth="depth"
        :class="[
          'group relative flex cursor-pointer items-start gap-2.5 border-b border-line px-3 py-2 last:border-0',
          'min-h-11 transition-colors hover:bg-subtle first:rounded-t-md last:rounded-b-md',
          draggingId === s.id && 'opacity-40',
          dropBeforeId === s.id && 'drop-before',
          dropAtEnd && isLast && depth === 0 && 'drop-after',
        ]"
        @dragover="depth === 0 && onRowDragOver(s.id, $event)"
        @drop.prevent="commitReorder"
      >
        <!-- Drag grip (top-level threads only): the one draggable handle, so the
             row's link still click/⌘-clicks. Drag within the list to reorder, or
             onto the Parked shelf below to rest the thread. Hover/focus-revealed. -->
        <button
          v-if="depth === 0"
          type="button"
          draggable="true"
          data-testid="session-drag"
          class="relative z-10 -ml-1 mt-1 shrink-0 cursor-grab text-faint opacity-0 transition-opacity hover:text-fg focus-visible:opacity-100 group-hover:opacity-100 active:cursor-grabbing"
          title="Drag to reorder, or onto Parked to rest"
          aria-label="Reorder or park this session"
          @dragstart="onDragStart(s.id, $event)"
          @dragend="onDragEnd"
          @click.prevent
        >
          ⠿
        </button>
        <span v-else class="-ml-1 w-3 shrink-0" aria-hidden="true"></span>

        <!-- Tree gutter: threads a child session under the one that launched it.
             Drawn only for nested rows, so a flat fleet is visually unchanged. -->
        <div v-if="depth > 0" class="tree-gutter" aria-hidden="true">
          <span
            v-for="(v, c) in verticals"
            :key="c"
            class="tree-col"
            :class="{ 'tree-col--through': v }"
          ></span>
          <span class="tree-col" :class="isLast ? 'tree-col--elbow' : 'tree-col--tee'"></span>
        </div>

        <!-- Status dot: a calm, scannable hue for the row's resolved state
             (green live · cyan resting · amber/red raised · faint detached). -->
        <span
          class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
          :class="lifecycleDot(s)"
          aria-hidden="true"
        ></span>

        <!-- Title + current-state (the work, in prose). -->
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <!-- Stretched link: the whole row is this anchor (see .stretched-link),
                 so right-click → "open in new tab", middle-click, and ⌘/Ctrl-click
                 all work. Interactive siblings below carry `relative z-10` to stay
                 clickable above the overlay. -->
            <router-link
              :to="`/s/${s.id}`"
              class="stretched-link truncate font-serif text-[15px] font-semibold text-fg hover:text-accent"
            >
              {{ s.branch.title || s.branch.name }}
            </router-link>
            <!-- Origin: the automation surface that launched this session
                 (github, slack, watch, actions, ops) — a quiet identity pill,
                 shown only for a non-human origin so an ordinary session
                 (`user`) stays unmarked. -->
            <span
              v-if="s.origin && s.origin !== 'user'"
              class="tag-pill"
              data-testid="origin-pill"
              :title="`origin: ${s.origin}`"
              >{{ s.origin }}</span
            >
            <!-- Loud signals: the agent's `attention` and a watch's
                 `triage`, each a deletable chip. The × clears that tag (calm is
                 its absence) — there is no separate "Mark OK" verb. -->
            <SignalChip
              v-for="chip in signalChips(s)"
              :key="chip.key"
              class="relative z-10"
              :chip="chip"
              :busy="clearingTag === `${s.id}:${chip.key}`"
              @clear="(key) => clearTag(s.id, key)"
            />
            <!-- Lifecycle: demoted, neutral, mono pill (StatusBadge). Hidden for
                 the running state — nearly every live row is running, so the pill
                 would just be repeated noise; only off-nominal states show one. -->
            <StatusBadge v-if="s.status !== 'running'" :status="s.status" class="shrink-0" />
            <!-- The cure, next to the diagnosis: an ORPHANED row offers Adopt, an
                 ARCHIVED one Recover, right where the badge announces the state.
                 Renders nothing for a healthy session. -->
            <SessionRemedyButton :ws="s" @changed="refresh" @error="error = $event" />
            <!-- Soothing idle mark: a calm, neutral chip when the agent is
                 resting (no loud signal). Reassures rather than alarms. -->
            <IdleChip v-if="idleTag(s)" :tag="idleTag(s)!" />
            <AgentUsage v-if="s.usage" :usage="s.usage" compact />
            <!-- Quiet free-form tags: deletable pills, never the loud fill. -->
            <TagPill
              v-for="t in quietTags(s)"
              :key="t.key"
              class="relative z-10"
              :tag="t"
              :busy="clearingTag === `${s.id}:${t.key}`"
              @clear="(key) => clearTag(s.id, key)"
            />
          </div>

          <!-- Current-state headline (agent's status message), else the goal —
               both in the serif prose voice. The status note is italic, a live
               margin annotation; the goal is roman and quieter beneath it. On an
               attention row the goal steps up from faint to muted so the metadata
               doesn't recede next to the loud chip. -->
          <p v-if="messageOf(s)" class="mt-0.5 truncate font-serif text-[13px] italic text-muted">
            {{ messageOf(s) }}
          </p>
          <p
            v-if="s.branch.goal"
            class="mt-0.5 truncate font-serif text-[13px]"
            :class="effectiveAttention(s).level === 'ok' ? 'text-faint' : 'text-muted'"
          >
            {{ s.branch.goal }}
          </p>
        </div>

        <!-- Ref: machine identity, mono, pushed far-right and receding. -->
        <div class="shrink-0 text-right">
          <span class="block truncate font-mono text-2xs text-faint">{{ s.branch.branch }}</span>
          <!-- Attribution: who/what launched this session — a subtle provenance
               label on the shared board. Absent for older rows. -->
          <span
            v-if="s.created_by"
            class="block truncate text-2xs text-faint"
            :title="`Launched by ${s.created_by}`"
          >
            by <span class="font-mono">{{ s.created_by }}</span>
          </span>
          <!-- PR snapshot (if any) — a quiet link straight to the GitHub PR. -->
          <GithubStatus
            v-if="s.branch.github"
            :gh="s.branch.github"
            compact
            class="relative z-10 mt-0.5 justify-end"
          />
          <router-link
            v-if="s.branch.open_issue_count"
            :to="`/s/${s.id}?tab=overview`"
            class="relative z-10 block font-mono text-2xs text-muted hover:text-accent"
            @click.stop
          >
            {{ s.branch.open_issue_count }} open issue{{
              s.branch.open_issue_count === 1 ? '' : 's'
            }}
          </router-link>
          <span v-if="s.last_activity_at" class="mt-0.5 block font-mono text-2xs text-faint">
            {{ timeAgo(s.last_activity_at) }}
          </span>
          <!-- The session brief: catch up without opening the terminal. The
               row's stretched link stays the shell — this is the side door. -->
          <router-link
            :to="`/s/${s.id}?tab=overview`"
            data-testid="row-overview"
            class="relative z-10 mt-0.5 block font-mono text-2xs text-faint hover:text-accent"
            @click.stop
          >
            overview →
          </router-link>
        </div>

        <!-- The row's ⋯ menu: park, then every lifecycle verb (Adopt/Recover,
             Archive, Remove). The fleet list is where a human surveys and tidies
             a fleet, so it acts here rather than only from inside a session. -->
        <SessionRowActions
          :ws="s"
          class="mt-0.5"
          @changed="refresh"
          @error="error = $event"
          @park="setPark(s.id, $event)"
        />
      </li>
    </ul>

    <!-- The Parked shelf — resting threads (long idle or parked by hand)
         collapsed out of the live list so a stale fleet doesn't drag the
         eye. Also a drop target: drag a live row here to rest it; the "Keep live"
         verb (and dragging a row back out) returns it. Shown while empty only
         mid-drag, so there's always somewhere to drop. -->
    <section
      v-if="pane === 'workspace' && (shelfCount || draggingId)"
      data-testid="parked-shelf"
      class="mt-3 rounded-md transition-shadow"
      :class="overShelf && draggingId ? 'shadow-[inset_0_0_0_1px_var(--accent)]' : ''"
      @dragover.prevent="draggingId && (overShelf = true)"
      @dragleave="overShelf = false"
      @drop.prevent="commitPark"
    >
      <button
        type="button"
        data-testid="parked-toggle"
        class="flex w-full items-center gap-2 px-1 py-1.5 text-2xs font-medium uppercase tracking-wider text-faint transition-colors hover:text-muted"
        @click="parkedOpen = !parkedOpen"
      >
        <span
          class="inline-block w-2 transition-transform"
          :class="parkedOpen || draggingId ? 'rotate-90' : ''"
          >▸</span
        >
        Parked
        <span class="font-serif text-[11px] normal-case italic tracking-normal text-faint"
          >resting — nothing for you</span
        >
        <span class="h-px flex-1 bg-line"></span>
        <span class="font-mono lowercase tracking-normal">{{ shelfCount }}</span>
      </button>

      <ul
        v-if="parkedOpen || draggingId"
        class="fade-in overflow-hidden rounded-md border border-line bg-surface"
      >
        <li
          v-for="{ session: s, depth } in shelfRows"
          :key="s.id"
          data-testid="parked-card"
          :data-session-id="s.id"
          class="group relative flex items-start gap-2.5 border-b border-line px-3 py-2 opacity-65 transition-opacity last:border-0 hover:bg-subtle hover:opacity-100"
        >
          <button
            v-if="depth === 0"
            type="button"
            draggable="true"
            data-testid="parked-drag"
            class="relative z-10 -ml-1 mt-1 shrink-0 cursor-grab text-faint opacity-0 transition-opacity hover:text-fg focus-visible:opacity-100 group-hover:opacity-100 active:cursor-grabbing"
            title="Drag back into the live list to keep it going"
            aria-label="Return this session to the live list"
            @dragstart="onDragStart(s.id, $event)"
            @dragend="onDragEnd"
            @click.prevent
          >
            ⠿
          </button>
          <span v-else class="-ml-1 w-3 shrink-0" aria-hidden="true"></span>

          <span
            class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
            :class="lifecycleDot(s)"
            aria-hidden="true"
          ></span>

          <div class="min-w-0 flex-1">
            <div class="flex flex-wrap items-center gap-2">
              <router-link
                :to="`/s/${s.id}`"
                class="stretched-link truncate font-serif text-[15px] font-medium text-fg hover:text-accent"
              >
                {{ s.branch.title || s.branch.name }}
              </router-link>
              <span
                v-if="s.origin && s.origin !== 'user'"
                class="tag-pill"
                data-testid="origin-pill"
                :title="`origin: ${s.origin}`"
                >{{ s.origin }}</span
              >
              <span class="meta-chip !text-[10px] uppercase tracking-wide text-info">{{
                parkLabel(s)
              }}</span>
            </div>
            <p v-if="s.branch.goal" class="mt-0.5 truncate font-serif text-[13px] text-faint">
              {{ s.branch.goal }}
            </p>
          </div>

          <div class="shrink-0 text-right">
            <span class="block truncate font-mono text-2xs text-faint">{{ s.branch.branch }}</span>
            <span v-if="s.last_activity_at" class="mt-0.5 block font-mono text-2xs text-faint">
              {{ timeAgo(s.last_activity_at) }}
            </span>
            <!-- A resting session is exactly the one you catch up on via the
                 brief rather than by waking its terminal. -->
            <router-link
              :to="`/s/${s.id}?tab=overview`"
              data-testid="row-overview"
              class="relative z-10 mt-0.5 block font-mono text-2xs text-faint hover:text-accent"
              @click.stop
            >
              overview →
            </router-link>
          </div>

          <button
            v-if="depth === 0"
            type="button"
            data-testid="parked-keep-live"
            class="relative z-10 mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-2xs text-muted opacity-0 transition-opacity hover:bg-subtle hover:text-fg focus-visible:opacity-100 group-hover:opacity-100"
            title="Return this session to the live list"
            @click.stop="setPark(s.id, 'active')"
          >
            Keep live
          </button>
        </li>

        <li
          v-if="!shelfRows.length"
          class="px-3 py-4 text-center font-serif text-[13px] italic text-faint"
        >
          Drop a session here to rest it.
        </li>
      </ul>
    </section>

    <AutomationSessions
      v-if="pane === 'automation'"
      :sessions="automationSessions"
      :fleet="sessions"
      :runs="runs"
      :history-open="historyOpen"
      :clearing-tag="clearingTag"
      @toggle-history="toggleAutomationHistory"
      @clear-tag="clearTag"
      @changed="refresh"
      @error="error = $event"
    />
  </div>
</template>

<style scoped>
/* Drag-reorder insertion indicator: a crisp accent rule on the edge the dragged
   thread would land against — top when dropping above a row, bottom past the
   last one. Inset so it sits inside the row's border without shifting layout. */
.drop-before {
  box-shadow: inset 0 2px 0 var(--accent);
}
.drop-after {
  box-shadow: inset 0 -2px 0 var(--accent);
}
</style>
