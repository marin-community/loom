<script setup lang="ts">
import { computed, nextTick, onActivated, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { Session, SessionGroup, SessionLayout, SessionSpace } from '../types';
import AutomationRunRow from '../components/AutomationRunRow.vue';
import ConfirmDialog from '../components/ConfirmDialog.vue';
import SessionWorkbenchRow from '../components/SessionWorkbenchRow.vue';
import {
  ApiError,
  createSessionGroup,
  createSessionSpace,
  del,
  deleteSessionGroup,
  deleteSessionSpace,
  moveSessions,
  post,
  reorderSessionLayout,
  restoreSessionGroups,
  searchSessions,
  setSessionGroupPreference,
  updateSessionGroup,
  updateSessionSpace,
} from '../api';
import {
  isAutomationRunHistory,
  unmatchedAutomationRuns,
  runNeedsIntervention,
} from '../lib/automationSessions';
import { effectiveAttention } from '../lib/sessionState';
import { useFleet } from '../lib/sessionsStore';
import { beginSessionOpen, recordSessionListReturn } from '../lib/workbenchMetrics';

defineOptions({ name: 'SessionList' });

const route = useRoute();
const router = useRouter();
const { sessions, runs, layout, refresh } = useFleet();
onActivated(recordSessionListReturn);

type WorkbenchView = 'space' | 'attention' | 'all' | 'history';

const liveSessions = computed(() =>
  sessions.value.filter((session) => session.status !== 'archived'),
);
const historySessions = computed(() =>
  sessions.value.filter((session) => session.status === 'archived'),
);
const attentionSessions = computed(() =>
  liveSessions.value.filter((session) => effectiveAttention(session).level !== 'ok'),
);

const view = computed<WorkbenchView>(() => {
  if (route.query.history === 'true') return 'history';
  if (route.query.view === 'attention') return 'attention';
  if (route.query.view === 'all') return 'all';
  return 'space';
});

const activeSpace = computed<SessionSpace | undefined>(() => {
  const spaces = layout.value?.spaces ?? [];
  if (!spaces.length) return undefined;
  const requested = typeof route.query.space === 'string' ? route.query.space : '';
  return (
    spaces.find((space) => space.id === requested) ??
    spaces.find((space) => space.system_key === 'user') ??
    spaces[0]
  );
});

function viewQuery(next: WorkbenchView, spaceId?: string) {
  const query = { ...route.query };
  delete query.view;
  delete query.history;
  delete query.space;
  delete query.new;
  if (next === 'attention') query.view = 'attention';
  if (next === 'all') query.view = 'all';
  if (next === 'history') query.history = 'true';
  if (next === 'space' && spaceId) query.space = spaceId;
  return query;
}

const statusFilter = ref(typeof route.query.status === 'string' ? route.query.status : '');
const attentionFilter = ref(typeof route.query.attention === 'string' ? route.query.attention : '');
watch(
  () => [route.query.status, route.query.attention],
  ([status, attention]) => {
    statusFilter.value = typeof status === 'string' ? status : '';
    attentionFilter.value = typeof attention === 'string' ? attention : '';
  },
);
function updateFilters() {
  router.replace({
    query: {
      ...route.query,
      status: statusFilter.value || undefined,
      attention: attentionFilter.value || undefined,
    },
  });
}

const searchText = ref('');
const includeHistory = ref(false);
const searchResults = ref<Session[] | null>(null);
const searching = ref(false);
let searchTimer: number | undefined;
let searchGeneration = 0;
let lastSearchArchivedOnly = view.value === 'history';
// Search membership is server-owned. Fleet/layout snapshots are replaced after
// polling, SSE invalidations, and mutations; while a query is open each fresh
// snapshot reruns the same guarded search so renamed, moved, created, and
// deleted sessions enter or leave the result set correctly.
watch(
  [searchText, includeHistory, () => view.value, sessions, layout],
  () => {
    window.clearTimeout(searchTimer);
    const generation = ++searchGeneration;
    const query = searchText.value.trim();
    const archivedOnly = view.value === 'history';
    if (archivedOnly !== lastSearchArchivedOnly) {
      // Never render results from the opposite route scope while the fresh
      // server-visible search is in flight.
      searchResults.value = null;
      lastSearchArchivedOnly = archivedOnly;
    }
    if (!query) {
      searchResults.value = null;
      searching.value = false;
      return;
    }
    searching.value = true;
    searchTimer = window.setTimeout(async () => {
      try {
        const result = await searchSessions(query, {
          history: archivedOnly || includeHistory.value,
          archivedOnly,
        });
        if (generation === searchGeneration) searchResults.value = result;
      } catch (cause) {
        if (generation === searchGeneration) error.value = (cause as Error).message;
      } finally {
        if (generation === searchGeneration) searching.value = false;
      }
    }, 160);
  },
  { flush: 'sync' },
);

function matchesFilters(session: Session): boolean {
  if (statusFilter.value && session.status !== statusFilter.value) return false;
  const attention = effectiveAttention(session).level;
  if (attentionFilter.value === 'needs' && attention === 'ok') return false;
  if (attentionFilter.value === 'ok' && attention !== 'ok') return false;
  if (attentionFilter.value === 'blocked' && attention !== 'blocked') return false;
  if (attentionFilter.value === 'attention' && attention !== 'attention') return false;
  return true;
}

const baseSessions = computed(() => {
  if (searchResults.value) {
    const current = new Map(sessions.value.map((session) => [session.id, session]));
    return searchResults.value
      .map((result) => current.get(result.id))
      .filter((session): session is Session => !!session);
  }
  if (view.value === 'history') return historySessions.value;
  if (view.value === 'attention') return attentionSessions.value;
  if (view.value === 'all') return liveSessions.value;
  const spaceId = activeSpace.value?.id;
  return liveSessions.value.filter((session) => session.placement?.space_id === spaceId);
});
const visibleSessions = computed(() => baseSessions.value.filter(matchesFilters));
const visibleById = computed(
  () => new Map(visibleSessions.value.map((session) => [session.id, session])),
);
const sessionsByBranch = computed(() =>
  sessions.value.reduce((byBranch, session) => {
    const candidates = byBranch.get(session.branch.id) ?? [];
    candidates.push(session);
    byBranch.set(session.branch.id, candidates);
    return byBranch;
  }, new Map<string, Session[]>()),
);
const sessionsById = computed(
  () => new Map(sessions.value.map((session) => [session.id, session])),
);
function parentSessionOf(session: Session) {
  if (session.parent_session_id) {
    return sessionsById.value.get(session.parent_session_id);
  }
  if (!session.parent_id) return undefined;
  const legacyCandidates = sessionsByBranch.value.get(session.parent_id) ?? [];
  return legacyCandidates.length === 1 ? legacyCandidates[0] : undefined;
}
const groupedSessions = computed(() =>
  (activeSpace.value?.groups ?? []).map((group) => ({
    group,
    sessions: group.session_ids
      .map((id) => visibleById.value.get(id))
      .filter((session): session is Session => !!session),
  })),
);
const smartSessions = computed(() =>
  [...visibleSessions.value].sort((left, right) => {
    if (view.value === 'attention') {
      const rank = { blocked: 0, attention: 1, ok: 2 };
      const difference =
        rank[effectiveAttention(left).level] - rank[effectiveAttention(right).level];
      if (difference) return difference;
    }
    const leftPlace = `${left.placement?.space_name ?? ''}/${left.placement?.group_name ?? ''}`;
    const rightPlace = `${right.placement?.space_name ?? ''}/${right.placement?.group_name ?? ''}`;
    return (
      leftPlace.localeCompare(rightPlace) ||
      (left.placement?.rank ?? 0) - (right.placement?.rank ?? 0)
    );
  }),
);
const failedRuns = computed(() =>
  unmatchedAutomationRuns(runs.value, sessions.value)
    .filter(runNeedsIntervention)
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at)),
);
const historicalRuns = computed(() =>
  unmatchedAutomationRuns(runs.value, sessions.value)
    .filter(isAutomationRunHistory)
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at)),
);
const showInterventions = computed(
  () =>
    view.value === 'attention' ||
    (view.value === 'space' && activeSpace.value?.system_key === 'ops'),
);

const expandedRows = ref(new Set<string>());
function rowExpanded(id: string) {
  return expandedRows.value.has(id);
}
function toggleRow(id: string) {
  const next = new Set(expandedRows.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expandedRows.value = next;
}

const selected = ref(new Set<string>());
watch(sessions, (next) => {
  const existing = new Set(next.map((session) => session.id));
  selected.value = new Set([...selected.value].filter((id) => existing.has(id)));
});
function isSelected(id: string) {
  return selected.value.has(id);
}
function setSelected(id: string, shouldSelect: boolean) {
  const next = new Set(selected.value);
  if (shouldSelect) next.add(id);
  else next.delete(id);
  selected.value = next;
}
function clearSelection() {
  selected.value = new Set();
}
const renderedVisibleIds = computed(() => {
  const ids = new Set(visibleById.value.keys());
  if (view.value !== 'space' || searchResults.value !== null) return ids;
  for (const { group } of allGroups.value) {
    if (!group.collapsed) continue;
    for (const id of group.session_ids) ids.delete(id);
  }
  return ids;
});
const visibleSelectedCount = computed(
  () => [...selected.value].filter((id) => renderedVisibleIds.value.has(id)).length,
);
const hiddenSelectedCount = computed(() => selected.value.size - visibleSelectedCount.value);

const allGroups = computed(() =>
  (layout.value?.spaces ?? []).flatMap((space) =>
    space.groups.map((group) => ({
      group,
      space,
      label: `${space.name} / ${group.name}`,
    })),
  ),
);
const bulkDestination = ref('');
watch(
  allGroups,
  (groups) => {
    if (!groups.some(({ group }) => group.id === bulkDestination.value)) {
      bulkDestination.value = groups[0]?.group.id ?? '';
    }
  },
  { immediate: true },
);

interface UndoGroup {
  group_id: string;
  session_ids: string[];
}
interface UndoMove {
  groups: UndoGroup[];
  message: string;
}
const undoMove = ref<UndoMove | null>(null);
const announcement = ref('');
const error = ref('');

function snapshotUndo(ids: string[], destinationGroupId: string): UndoMove {
  const moving = new Set(ids);
  const affected = new Set([destinationGroupId]);
  for (const { group } of allGroups.value) {
    if (group.session_ids.some((id) => moving.has(id))) affected.add(group.id);
  }
  const groups = allGroups.value
    .filter(({ group }) => affected.has(group.id))
    .map(({ group }) => ({ group_id: group.id, session_ids: [...group.session_ids] }));
  const destination = allGroups.value.find(
    (candidate) => candidate.group.id === destinationGroupId,
  );
  return {
    groups,
    message: `Moved ${ids.length} session${ids.length === 1 ? '' : 's'} to ${
      destination?.label ?? destinationGroupId
    }`,
  };
}

function useConflictLayout(cause: unknown) {
  if (!(cause instanceof ApiError) || cause.status !== 409) return false;
  const current = cause.body.layout;
  if (current && typeof current === 'object') layout.value = current as SessionLayout;
  error.value =
    'The workbench changed in another client. Review the refreshed layout and try again.';
  return true;
}

async function performMove(
  ids: string[],
  destinationGroupId: string,
  beforeSessionId: string | null = null,
  reversible = true,
) {
  if (!ids.length || !layout.value) return;
  error.value = '';
  const undo = snapshotUndo(ids, destinationGroupId);
  try {
    layout.value = await moveSessions({
      session_ids: ids,
      destination_group_id: destinationGroupId,
      before_session_id: beforeSessionId,
      expected_revision: layout.value.revision,
    });
    if (reversible) undoMove.value = undo;
    announcement.value = undo.message;
    clearSelection();
    await refresh();
    await nextTick();
    const movedRow =
      ids.length === 1
        ? document.querySelector<HTMLElement>(
            `[data-session-id="${CSS.escape(ids[0])}"] [data-testid="move-session"]`,
          )
        : null;
    const focusableMovedRow =
      movedRow && movedRow.offsetParent !== null && !movedRow.hasAttribute('disabled')
        ? movedRow
        : null;
    (focusableMovedRow ?? undoButton.value)?.focus();
  } catch (cause) {
    if (!useConflictLayout(cause)) error.value = (cause as Error).message;
    await refresh();
  }
}

async function restoreMove() {
  const undo = undoMove.value;
  if (!undo) return;
  error.value = '';
  try {
    if (!layout.value) return;
    layout.value = await restoreSessionGroups({
      groups: undo.groups,
      expected_revision: layout.value.revision,
    });
    undoMove.value = null;
    announcement.value = 'Move undone';
    await refresh();
  } catch (cause) {
    if (!useConflictLayout(cause)) error.value = `Could not undo move: ${(cause as Error).message}`;
    await refresh();
  }
}

const moveOpenId = ref('');
const rowDestination = ref<Record<string, string>>({});
const rowBefore = ref<Record<string, string>>({});
function openMove(session: Session) {
  moveOpenId.value = moveOpenId.value === session.id ? '' : session.id;
  rowDestination.value[session.id] =
    rowDestination.value[session.id] || session.placement?.group_id || allGroups.value[0]?.group.id;
  rowBefore.value[session.id] = '';
}

const draggingId = ref('');
const dropGroupId = ref('');
const dropBeforeId = ref('');
function dragStart(id: string, event: DragEvent) {
  draggingId.value = id;
  event.dataTransfer?.setData('text/plain', id);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
}
function dragOver(groupId: string, beforeId = '') {
  if (!draggingId.value) return;
  dropGroupId.value = groupId;
  dropBeforeId.value = beforeId;
}
function clearDrag() {
  draggingId.value = '';
  dropGroupId.value = '';
  dropBeforeId.value = '';
}
async function drop(groupId: string, beforeId: string | null = null) {
  const id = draggingId.value;
  clearDrag();
  if (id) await performMove([id], groupId, beforeId);
}

async function toggleGroup(group: SessionGroup) {
  const previous = group.collapsed;
  group.collapsed = !previous;
  try {
    layout.value = await setSessionGroupPreference(group.id, group.collapsed);
  } catch (cause) {
    group.collapsed = previous;
    error.value = (cause as Error).message;
  }
}

const organizeOpen = ref(false);
const organizeSpaceId = ref('');
const newSpaceName = ref('');
const newGroupName = ref('');
const spaceNames = ref<Record<string, string>>({});
const groupNames = ref<Record<string, string>>({});
const deleteDestinations = ref<Record<string, string>>({});
const groupDestinationSpaces = ref<Record<string, string>>({});
const pendingDelete = ref<
  { kind: 'space'; item: SessionSpace } | { kind: 'group'; item: SessionGroup } | null
>(null);
watch(
  layout,
  (next) => {
    if (!next) return;
    if (!next.spaces.some((space) => space.id === organizeSpaceId.value)) {
      organizeSpaceId.value = activeSpace.value?.id ?? next.spaces[0]?.id ?? '';
    }
    for (const space of next.spaces) {
      spaceNames.value[space.id] ??= space.name;
      for (const group of space.groups) {
        groupNames.value[group.id] ??= group.name;
        groupDestinationSpaces.value[group.id] ??= space.id;
      }
    }
  },
  { immediate: true },
);
const organizeSpace = computed(() =>
  layout.value?.spaces.find((space) => space.id === organizeSpaceId.value),
);

async function mutateLayout(operation: () => Promise<SessionLayout>) {
  error.value = '';
  try {
    layout.value = await operation();
    await refresh();
  } catch (cause) {
    if (!useConflictLayout(cause)) error.value = (cause as Error).message;
    await refresh();
  }
}
function addSpace() {
  if (!layout.value || !newSpaceName.value.trim()) return;
  const revision = layout.value.revision;
  const name = newSpaceName.value;
  void mutateLayout(async () => {
    const next = await createSessionSpace(name, revision);
    newSpaceName.value = '';
    return next;
  });
}
function renameSpace(space: SessionSpace) {
  if (!layout.value) return;
  void mutateLayout(() =>
    updateSessionSpace(space.id, spaceNames.value[space.id], layout.value!.revision),
  );
}
function removeSpace(space: SessionSpace) {
  if (!layout.value) return;
  void mutateLayout(() =>
    deleteSessionSpace(
      space.id,
      deleteDestinations.value[space.id] || null,
      layout.value!.revision,
    ),
  );
}
function addGroup(space: SessionSpace) {
  if (!layout.value || !newGroupName.value.trim()) return;
  const revision = layout.value.revision;
  const name = newGroupName.value;
  void mutateLayout(async () => {
    const next = await createSessionGroup(space.id, name, revision);
    newGroupName.value = '';
    return next;
  });
}
function renameGroup(group: SessionGroup) {
  if (!layout.value) return;
  void mutateLayout(() =>
    updateSessionGroup(group.id, groupNames.value[group.id], layout.value!.revision),
  );
}
function removeGroup(group: SessionGroup) {
  if (!layout.value) return;
  void mutateLayout(() =>
    deleteSessionGroup(
      group.id,
      deleteDestinations.value[group.id] || null,
      layout.value!.revision,
    ),
  );
}
function confirmLayoutDelete() {
  const pending = pendingDelete.value;
  pendingDelete.value = null;
  if (!pending) return;
  if (pending.kind === 'space') removeSpace(pending.item);
  else removeGroup(pending.item);
}
function reorderSpace(space: SessionSpace, direction: -1 | 1) {
  if (!layout.value) return;
  const spaces = layout.value.spaces;
  const index = spaces.findIndex((candidate) => candidate.id === space.id);
  const target = index + direction;
  if (target < 0 || target >= spaces.length) return;
  const beforeId = direction < 0 ? spaces[target].id : spaces[target + 1]?.id;
  void mutateLayout(() =>
    reorderSessionLayout({
      kind: 'space',
      id: space.id,
      before_id: beforeId ?? null,
      expected_revision: layout.value!.revision,
    }),
  );
}
function reorderGroup(group: SessionGroup, direction: -1 | 1) {
  if (!layout.value) return;
  const groups = organizeSpace.value?.groups ?? [];
  const index = groups.findIndex((candidate) => candidate.id === group.id);
  const target = index + direction;
  if (target < 0 || target >= groups.length) return;
  const beforeId = direction < 0 ? groups[target].id : groups[target + 1]?.id;
  void mutateLayout(() =>
    reorderSessionLayout({
      kind: 'group',
      id: group.id,
      before_id: beforeId ?? null,
      destination_space_id: group.space_id,
      expected_revision: layout.value!.revision,
    }),
  );
}
function moveGroupToSpace(group: SessionGroup) {
  if (!layout.value) return;
  const destination = groupDestinationSpaces.value[group.id];
  if (!destination || destination === group.space_id) return;
  void mutateLayout(() =>
    reorderSessionLayout({
      kind: 'group',
      id: group.id,
      before_id: null,
      destination_space_id: destination,
      expected_revision: layout.value!.revision,
    }),
  );
}

const pendingBulkArchive = ref(false);
const archiveResult = ref('');
async function confirmBulkArchive() {
  pendingBulkArchive.value = false;
  error.value = '';
  const ids = [...selected.value];
  const results = await Promise.allSettled(ids.map((id) => post(`/sessions/${id}/archive`)));
  const failed = ids.filter((_, index) => results[index].status === 'rejected');
  const archived = ids.length - failed.length;
  selected.value = new Set(failed);
  announcement.value = `Archived ${archived} session${archived === 1 ? '' : 's'}${
    failed.length ? `; ${failed.length} failed and remain selected` : ''
  }`;
  archiveResult.value = announcement.value;
  if (failed.length) error.value = announcement.value;
  await refresh();
}

const undoButton = ref<HTMLButtonElement>();

function openForm() {
  router.push('/sessions/new');
}

const clearingTag = ref('');
async function clearTag(sessionId: string, key: string) {
  clearingTag.value = `${sessionId}:${key}`;
  try {
    await del(`/sessions/${sessionId}/tags/${encodeURIComponent(key)}`);
    await refresh();
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    clearingTag.value = '';
  }
}

function recordSessionLinkOpen(event: MouseEvent, sessionId: string) {
  if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
    return;
  beginSessionOpen(sessionId);
}
function scrollSpaces(direction: number) {
  document
    .querySelector('[data-testid="space-tabs-scroll"]')
    ?.scrollBy({ left: direction * 240, behavior: 'smooth' });
}
</script>

<template>
  <div class="px-5 py-3">
    <h1 class="sr-only">Sessions</h1>

    <div class="mb-3 flex flex-wrap items-center gap-2">
      <label class="relative min-w-52 flex-1 sm:max-w-md">
        <span class="sr-only">Search sessions</span>
        <input
          v-model="searchText"
          type="search"
          data-testid="fleet-search"
          placeholder="Search sessions, prompts, repos, issues…"
          class="w-full rounded border border-line bg-input px-3 py-1.5 text-sm outline-none focus:ring-1 focus:ring-accent"
        />
        <span v-if="searching" class="absolute right-2 top-2 text-2xs text-faint">searching…</span>
      </label>
      <label v-if="view !== 'history'" class="flex items-center gap-1.5 text-xs text-muted">
        <input v-model="includeHistory" type="checkbox" data-testid="search-history" />
        include History
      </label>
      <select
        v-model="statusFilter"
        aria-label="Filter by status"
        data-testid="status-filter"
        class="rounded border border-line bg-input px-2 py-1.5 text-xs"
        @change="updateFilters"
      >
        <option value="">Any status</option>
        <option
          v-for="status in ['running', 'created', 'orphaned', 'error', 'done', 'archived']"
          :key="status"
        >
          {{ status }}
        </option>
      </select>
      <select
        v-model="attentionFilter"
        aria-label="Filter by attention"
        data-testid="attention-filter"
        class="rounded border border-line bg-input px-2 py-1.5 text-xs"
        @change="updateFilters"
      >
        <option value="">Any attention</option>
        <option value="needs">Needs attention</option>
        <option value="blocked">Blocked</option>
        <option value="attention">Attention</option>
        <option value="ok">Calm</option>
      </select>
      <button
        class="btn-secondary px-2.5 py-1 text-xs"
        type="button"
        @click="organizeOpen = !organizeOpen"
      >
        Organize
      </button>
      <button
        v-if="view !== 'history'"
        class="btn-primary ml-auto px-2.5 py-1 text-xs font-medium"
        type="button"
        @click="openForm"
      >
        New session
      </button>
    </div>

    <div class="mb-3 flex min-w-0 items-stretch gap-1">
      <button
        class="btn-secondary px-2 text-xs"
        aria-label="Scroll spaces left"
        @click="scrollSpaces(-1)"
      >
        ‹
      </button>
      <nav
        data-testid="space-tabs-scroll"
        aria-label="Session workbench views"
        class="flex min-w-0 flex-1 gap-1 overflow-x-auto rounded border border-line bg-surface p-1"
      >
        <router-link
          :to="{ path: '/', query: viewQuery('attention') }"
          data-testid="attention-view"
          :aria-current="view === 'attention' ? 'page' : undefined"
          :class="view === 'attention' ? 'bg-attn-soft text-attn' : 'text-muted hover:bg-subtle'"
          class="flex shrink-0 items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium"
        >
          Attention
          <span class="font-mono text-2xs">{{ attentionSessions.length + failedRuns.length }}</span>
        </router-link>
        <router-link
          v-for="space in layout?.spaces ?? []"
          :key="space.id"
          :to="{ path: '/', query: viewQuery('space', space.id) }"
          :data-space-id="space.id"
          :aria-current="view === 'space' && activeSpace?.id === space.id ? 'page' : undefined"
          :class="
            view === 'space' && activeSpace?.id === space.id
              ? 'bg-accent text-accent-fg'
              : 'text-muted hover:bg-subtle'
          "
          class="flex shrink-0 items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium"
        >
          {{ space.name }}
          <span class="font-mono text-2xs">
            {{ liveSessions.filter((session) => session.placement?.space_id === space.id).length }}
          </span>
        </router-link>
        <router-link
          :to="{ path: '/', query: viewQuery('all') }"
          data-testid="all-view"
          :aria-current="view === 'all' ? 'page' : undefined"
          :class="view === 'all' ? 'bg-accent text-accent-fg' : 'text-muted hover:bg-subtle'"
          class="flex shrink-0 items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium"
        >
          All <span class="font-mono text-2xs">{{ liveSessions.length }}</span>
        </router-link>
        <router-link
          :to="{ path: '/', query: viewQuery('history') }"
          data-testid="history-view"
          :aria-current="view === 'history' ? 'page' : undefined"
          :class="view === 'history' ? 'bg-accent text-accent-fg' : 'text-muted hover:bg-subtle'"
          class="flex shrink-0 items-center gap-1.5 rounded px-2.5 py-1 text-xs font-medium"
        >
          History
          <span class="font-mono text-2xs">{{
            historySessions.length + historicalRuns.length
          }}</span>
        </router-link>
      </nav>
      <button
        class="btn-secondary px-2 text-xs"
        aria-label="Scroll spaces right"
        @click="scrollSpaces(1)"
      >
        ›
      </button>
    </div>

    <section
      v-if="organizeOpen && layout"
      data-testid="layout-organizer"
      class="mb-4 rounded border border-line bg-surface p-3"
    >
      <div class="flex flex-wrap items-center gap-2">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-muted">Layout</h2>
        <select
          v-model="organizeSpaceId"
          aria-label="Space to organize"
          class="rounded bg-input px-2 py-1 text-xs"
        >
          <option v-for="space in layout.spaces" :key="space.id" :value="space.id">
            {{ space.name }}
          </option>
        </select>
        <input
          v-model="newSpaceName"
          placeholder="New space"
          class="rounded bg-input px-2 py-1 text-xs"
        />
        <button class="btn-secondary px-2 py-1 text-xs" type="button" @click="addSpace">
          Add space
        </button>
      </div>

      <div v-if="organizeSpace" class="mt-3 space-y-2">
        <div class="flex flex-wrap items-center gap-1.5">
          <input
            v-model="spaceNames[organizeSpace.id]"
            aria-label="Space name"
            class="rounded bg-input px-2 py-1 text-xs"
          />
          <button
            class="btn-secondary px-2 py-1 text-xs"
            type="button"
            @click="renameSpace(organizeSpace)"
          >
            Rename
          </button>
          <button
            class="btn-secondary px-2 py-1 text-xs"
            type="button"
            aria-label="Move space left"
            @click="reorderSpace(organizeSpace, -1)"
          >
            ←
          </button>
          <button
            class="btn-secondary px-2 py-1 text-xs"
            type="button"
            aria-label="Move space right"
            @click="reorderSpace(organizeSpace, 1)"
          >
            →
          </button>
          <select
            v-model="deleteDestinations[organizeSpace.id]"
            aria-label="Destination when deleting space"
            class="ml-auto rounded bg-input px-2 py-1 text-xs"
          >
            <option value="">Destination if needed…</option>
            <option
              v-for="entry in allGroups.filter((entry) => entry.space.id !== organizeSpace?.id)"
              :key="entry.group.id"
              :value="entry.group.id"
            >
              {{ entry.label }}
            </option>
          </select>
          <button
            class="rounded px-2 py-1 text-xs text-block hover:bg-block-soft"
            type="button"
            @click="pendingDelete = { kind: 'space', item: organizeSpace }"
          >
            Delete space
          </button>
        </div>
        <div class="flex items-center gap-1.5">
          <input
            v-model="newGroupName"
            placeholder="New group"
            class="rounded bg-input px-2 py-1 text-xs"
          />
          <button
            class="btn-secondary px-2 py-1 text-xs"
            type="button"
            @click="addGroup(organizeSpace)"
          >
            Add empty group
          </button>
        </div>
        <ul class="divide-y divide-line rounded border border-line">
          <li
            v-for="group in organizeSpace.groups"
            :key="group.id"
            class="flex flex-wrap items-center gap-1.5 p-2"
          >
            <input
              v-model="groupNames[group.id]"
              :aria-label="`Name for ${group.name}`"
              class="rounded bg-input px-2 py-1 text-xs"
            />
            <span class="font-mono text-2xs text-faint"
              >{{ group.session_ids.length }} sessions</span
            >
            <button
              class="btn-secondary px-2 py-1 text-xs"
              type="button"
              @click="renameGroup(group)"
            >
              Rename
            </button>
            <button
              class="btn-secondary px-2 py-1 text-xs"
              type="button"
              :aria-label="`Move ${group.name} up`"
              @click="reorderGroup(group, -1)"
            >
              ↑
            </button>
            <button
              class="btn-secondary px-2 py-1 text-xs"
              type="button"
              :aria-label="`Move ${group.name} down`"
              @click="reorderGroup(group, 1)"
            >
              ↓
            </button>
            <select
              v-model="groupDestinationSpaces[group.id]"
              :aria-label="`Destination space for ${group.name}`"
              class="rounded bg-input px-2 py-1 text-xs"
            >
              <option v-for="space in layout.spaces" :key="space.id" :value="space.id">
                {{ space.name }}
              </option>
            </select>
            <button
              class="btn-secondary px-2 py-1 text-xs"
              type="button"
              :disabled="groupDestinationSpaces[group.id] === group.space_id"
              @click="moveGroupToSpace(group)"
            >
              Move group
            </button>
            <select
              v-model="deleteDestinations[group.id]"
              :aria-label="`Destination when deleting ${group.name}`"
              class="ml-auto rounded bg-input px-2 py-1 text-xs"
            >
              <option value="">Destination if needed…</option>
              <option
                v-for="entry in allGroups.filter((entry) => entry.group.id !== group.id)"
                :key="entry.group.id"
                :value="entry.group.id"
              >
                {{ entry.label }}
              </option>
            </select>
            <button
              class="rounded px-2 py-1 text-xs text-block hover:bg-block-soft"
              type="button"
              @click="pendingDelete = { kind: 'group', item: group }"
            >
              Delete
            </button>
          </li>
        </ul>
      </div>
    </section>

    <div
      v-if="error"
      role="alert"
      class="mb-3 rounded border border-block-line/40 bg-block-soft px-3 py-2 text-sm text-block"
    >
      {{ error }}
    </div>
    <div
      v-if="archiveResult"
      data-testid="archive-result"
      role="status"
      class="mb-3 rounded border border-line bg-surface px-3 py-2 text-sm text-muted"
    >
      {{ archiveResult }}
    </div>

    <div
      v-if="selected.size"
      data-testid="selection-toolbar"
      class="sticky top-2 z-20 mb-3 flex flex-wrap items-center gap-2 rounded border border-accent bg-surface px-3 py-2 shadow"
    >
      <strong class="text-xs">
        {{ selected.size }} selected
        <span v-if="hiddenSelectedCount" class="font-normal text-muted">
          · {{ hiddenSelectedCount }} hidden by this view
        </span>
      </strong>
      <select
        v-model="bulkDestination"
        aria-label="Move selected to group"
        class="rounded bg-input px-2 py-1 text-xs"
      >
        <option v-for="entry in allGroups" :key="entry.group.id" :value="entry.group.id">
          {{ entry.label }}
        </option>
      </select>
      <button
        class="btn-primary px-2 py-1 text-xs"
        type="button"
        @click="performMove([...selected], bulkDestination)"
      >
        Move
      </button>
      <button
        class="btn-secondary px-2 py-1 text-xs"
        type="button"
        @click="pendingBulkArchive = true"
      >
        Archive
      </button>
      <button class="btn-secondary px-2 py-1 text-xs" type="button" @click="clearSelection">
        Clear
      </button>
    </div>

    <section v-if="showInterventions" class="mb-4" aria-labelledby="interventions-heading">
      <div class="mb-1.5 flex items-center gap-2">
        <h2
          id="interventions-heading"
          class="text-2xs font-semibold uppercase tracking-wider text-muted"
        >
          Interventions
        </h2>
        <span class="rounded-full bg-block-soft px-1.5 font-mono text-2xs text-block">{{
          failedRuns.length
        }}</span>
      </div>
      <ul
        v-if="failedRuns.length"
        data-testid="interventions"
        class="rounded border border-line bg-surface"
      >
        <AutomationRunRow
          v-for="run in failedRuns"
          :key="run.id"
          :run="run"
          intervention
          @changed="refresh"
          @error="error = $event"
        />
      </ul>
      <p v-else class="rounded border border-dashed border-line px-3 py-3 text-sm text-muted">
        No failed runs need intervention.
      </p>
    </section>

    <template v-if="view === 'space' && searchResults === null">
      <section
        v-for="{ group, sessions: groupSessions } in groupedSessions"
        :key="group.id"
        :data-group-id="group.id"
        data-testid="session-group"
        class="mb-3 rounded border border-line bg-surface"
        :class="dropGroupId === group.id && !dropBeforeId ? 'ring-1 ring-accent' : ''"
        @dragover.prevent="dragOver(group.id)"
        @drop.prevent="drop(group.id)"
      >
        <header class="flex min-h-9 items-center gap-2 border-b border-line px-3 py-1.5">
          <button
            type="button"
            :aria-expanded="!group.collapsed"
            :aria-controls="`group-${group.id}`"
            :aria-label="`${group.collapsed ? 'Expand' : 'Collapse'} ${group.name}`"
            class="flex min-w-0 flex-1 items-center gap-2 text-left"
            @click="toggleGroup(group)"
          >
            <span class="text-faint" :class="group.collapsed ? '' : 'rotate-90'">▸</span>
            <span class="truncate text-sm font-semibold">{{ group.name }}</span>
            <span class="font-mono text-2xs text-faint">{{ groupSessions.length }}</span>
          </button>
        </header>
        <ul v-show="!group.collapsed" :id="`group-${group.id}`" data-testid="session-list">
          <SessionWorkbenchRow
            v-for="session in groupSessions"
            :key="session.id"
            :session="session"
            :qualified="false"
            :selected="isSelected(session.id)"
            :expanded="rowExpanded(session.id)"
            :move-open="moveOpenId === session.id"
            :destination="rowDestination[session.id]"
            :before="rowBefore[session.id]"
            :all-groups="allGroups"
            :all-sessions="sessions"
            :parent-session="parentSessionOf(session)"
            :dragging="draggingId === session.id"
            :drop-before="dropGroupId === group.id && dropBeforeId === session.id"
            :clearing-tag="clearingTag"
            @toggle-select="setSelected(session.id, $event)"
            @toggle-details="toggleRow(session.id)"
            @open-move="openMove(session)"
            @update-destination="rowDestination[session.id] = $event"
            @update-before="rowBefore[session.id] = $event"
            @move="performMove([session.id], $event.groupId, $event.beforeId || null)"
            @drag-start="dragStart(session.id, $event)"
            @drag-end="clearDrag"
            @drag-over="dragOver(group.id, session.id)"
            @drop="drop(group.id, session.id)"
            @changed="refresh"
            @error="error = $event"
            @clear-tag="clearTag(session.id, $event)"
            @record-open="recordSessionLinkOpen($event, session.id)"
          />
          <li
            v-if="!groupSessions.length"
            data-testid="empty-group"
            class="px-3 py-5 text-center text-sm text-faint"
          >
            {{
              group.session_ids.length
                ? 'No sessions match the current view or filters.'
                : 'Empty group — drop sessions here or use Move.'
            }}
          </li>
        </ul>
      </section>
    </template>

    <ul
      v-else-if="smartSessions.length"
      data-testid="session-list"
      class="rounded border border-line bg-surface"
    >
      <SessionWorkbenchRow
        v-for="session in smartSessions"
        :key="session.id"
        :session="session"
        qualified
        :selected="isSelected(session.id)"
        :expanded="rowExpanded(session.id)"
        :move-open="moveOpenId === session.id"
        :destination="rowDestination[session.id]"
        :before="rowBefore[session.id]"
        :all-groups="allGroups"
        :all-sessions="sessions"
        :parent-session="parentSessionOf(session)"
        :dragging="draggingId === session.id"
        :drop-before="dropGroupId === session.placement?.group_id && dropBeforeId === session.id"
        :clearing-tag="clearingTag"
        @toggle-select="setSelected(session.id, $event)"
        @toggle-details="toggleRow(session.id)"
        @open-move="openMove(session)"
        @update-destination="rowDestination[session.id] = $event"
        @update-before="rowBefore[session.id] = $event"
        @move="performMove([session.id], $event.groupId, $event.beforeId || null)"
        @drag-start="dragStart(session.id, $event)"
        @drag-end="clearDrag"
        @drag-over="dragOver(session.placement?.group_id ?? '', session.id)"
        @drop="drop(session.placement?.group_id ?? '', session.id)"
        @changed="refresh"
        @error="error = $event"
        @clear-tag="clearTag(session.id, $event)"
        @record-open="recordSessionLinkOpen($event, session.id)"
      />
    </ul>

    <section v-if="view === 'history' && historicalRuns.length" class="mt-4">
      <h2 class="mb-1.5 text-2xs font-semibold uppercase tracking-wider text-muted">
        Automation run history
      </h2>
      <ul data-testid="automation-run-history" class="rounded border border-line bg-surface">
        <AutomationRunRow
          v-for="run in historicalRuns"
          :key="run.id"
          :run="run"
          :intervention="false"
          history
          @changed="refresh"
          @error="error = $event"
        />
      </ul>
    </section>

    <div
      v-if="
        (view !== 'space' || searchResults !== null) &&
        !smartSessions.length &&
        !(view === 'history' && historicalRuns.length) &&
        !(showInterventions && failedRuns.length)
      "
      class="rounded border border-dashed border-line p-6 text-center"
    >
      <p class="text-sm text-muted">
        {{
          view === 'history'
            ? historySessions.length
              ? 'No archived sessions match this search or the current filters.'
              : 'No archived sessions yet.'
            : searchText
              ? 'No sessions match this search.'
              : 'No actionable sessions here.'
        }}
      </p>
    </div>

    <div
      v-if="undoMove"
      data-testid="move-undo"
      role="status"
      class="fixed bottom-10 right-4 z-40 flex items-center gap-3 rounded border border-line bg-surface px-3 py-2 text-sm shadow-lg"
    >
      <span>{{ undoMove.message }}</span>
      <button
        ref="undoButton"
        type="button"
        class="font-semibold text-accent hover:underline"
        @click="restoreMove"
      >
        Undo
      </button>
      <button
        type="button"
        class="text-faint hover:text-fg"
        aria-label="Dismiss move notice"
        @click="undoMove = null"
      >
        ×
      </button>
    </div>
    <p class="sr-only" aria-live="polite">{{ announcement }}</p>
    <ConfirmDialog
      :open="pendingDelete !== null"
      :title="`Delete ${pendingDelete?.kind ?? 'layout item'}?`"
      :description="
        pendingDelete?.kind === 'space'
          ? 'The space and its groups are removed. Any sessions or placement defaults move to the selected destination.'
          : 'The group is removed. Any sessions or placement defaults move to the selected destination.'
      "
      confirm-label="Delete"
      danger
      @confirm="confirmLayoutDelete"
      @cancel="pendingDelete = null"
    />
    <ConfirmDialog
      :open="pendingBulkArchive"
      title="Archive selected sessions?"
      :description="`${selected.size} selected session${selected.size === 1 ? '' : 's'} will be torn down and kept in History. Failures remain selected so they can be retried.`"
      confirm-label="Archive selected"
      @confirm="confirmBulkArchive"
      @cancel="pendingBulkArchive = false"
    />
  </div>
</template>
