<script setup lang="ts">
import { computed, nextTick, onActivated, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type {
  AutomationRun,
  Session,
  SessionCreatorFilter,
  SessionGroup,
  SessionSearchAttention,
  SessionSearchStatus,
  SessionSummary,
  SessionSpace,
} from '../types';
import { me } from '../auth';
import AutomationRunRow from '../components/AutomationRunRow.vue';
import ConfirmDialog from '../components/ConfirmDialog.vue';
import SessionMailboxPreview from '../components/SessionMailboxPreview.vue';
import SessionWorkbenchRow from '../components/SessionWorkbenchRow.vue';
import {
  archiveSession,
  clearSessionTag,
  createSessionGroup,
  createSessionSpace,
  deleteSessionGroup,
  deleteSessionSpace,
  getSession,
  listSessionSummaries,
  moveSessions,
  reorderSessionLayout,
  restoreSessionGroups,
  setSessionGroupPreference,
  updateSessionGroup,
  updateSessionSpace,
} from '../api';
import { unmatchedAutomationRuns, unmatchedRunProjection } from '../lib/automationSessions';
import { effectiveAttention } from '../lib/sessionState';
import { useLayoutCommands } from '../lib/layoutCommands';
import { useFleet } from '../lib/sessionsStore';
import { beginSessionOpen, recordSessionListReturn } from '../lib/workbenchMetrics';
import { useCommandScope, type Command } from '../lib/commands';

defineOptions({ name: 'SessionList' });

const route = useRoute();
const router = useRouter();
const { sessions, runs, layout, resourceErrors, refresh, loadHistory, focusSession } = useFleet();

type WorkbenchView = 'space' | 'attention' | 'all' | 'history';
type SessionSort = 'manual' | 'name' | 'activity' | 'created';
interface SessionTreeRow {
  session: SessionSummary;
  depth: number;
}

const liveSessions = computed(() =>
  sessions.value.filter((session) => session.status !== 'archived'),
);
const historySessions = computed(() =>
  sessions.value.filter((session) => session.status === 'archived'),
);
const view = computed<WorkbenchView>(() => {
  if (route.query.history === 'true') return 'history';
  if (route.query.view === 'attention') return 'attention';
  if (route.query.view === 'all') return 'all';
  return 'space';
});
watch(
  view,
  (next) => {
    if (next === 'history') void loadHistory();
  },
  { immediate: true },
);

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

const SEARCH_STATUSES = new Set<SessionSearchStatus>([
  'created',
  'running',
  'orphaned',
  'done',
  'error',
  'archived',
]);
const SEARCH_ATTENTION = new Set<SessionSearchAttention>(['needs', 'ok', 'attention', 'blocked']);
function enumFromQuery<T extends string>(value: unknown, allowed: Set<T>): T | undefined {
  return typeof value === 'string' && allowed.has(value as T) ? (value as T) : undefined;
}
function statusFromQuery(value: unknown): SessionSearchStatus | '' {
  return enumFromQuery(value, SEARCH_STATUSES) ?? '';
}
function attentionFromQuery(value: unknown): SessionSearchAttention | '' {
  return enumFromQuery(value, SEARCH_ATTENTION) ?? '';
}
const SESSION_SORTS = new Set<SessionSort>(['manual', 'name', 'activity', 'created']);
const CREATOR_FILTERS = new Set<SessionCreatorFilter>([
  'mine',
  'ops',
  'mine-and-ops',
  'other-users',
]);
function sortFromQuery(value: unknown): SessionSort {
  return enumFromQuery(value, SESSION_SORTS) ?? 'manual';
}
function creatorFromQuery(value: unknown): SessionCreatorFilter | '' {
  return enumFromQuery(value, CREATOR_FILTERS) ?? '';
}
const statusFilter = ref<SessionSearchStatus | ''>(statusFromQuery(route.query.status));
const attentionFilter = ref<SessionSearchAttention | ''>(attentionFromQuery(route.query.attention));
const creatorFilter = ref<SessionCreatorFilter | ''>(creatorFromQuery(route.query.creator));
const sessionSort = ref<SessionSort>(sortFromQuery(route.query.sort));
watch(
  () => [route.query.status, route.query.attention, route.query.creator, route.query.sort],
  ([status, attention, creator, sort]) => {
    statusFilter.value = statusFromQuery(status);
    attentionFilter.value = attentionFromQuery(attention);
    creatorFilter.value = creatorFromQuery(creator);
    sessionSort.value = sortFromQuery(sort);
  },
);
function updateFilters() {
  router.replace({
    query: {
      ...route.query,
      status: statusFilter.value || undefined,
      attention: attentionFilter.value || undefined,
      creator: creatorFilter.value || undefined,
    },
  });
}
function updateSort() {
  router.replace({
    query: {
      ...route.query,
      sort: sessionSort.value === 'manual' ? undefined : sessionSort.value,
    },
  });
}

const searchText = ref('');
const searchInput = ref<HTMLInputElement>();
const includeHistory = ref(false);
const searchResults = ref<SessionSummary[] | null>(null);
const searching = ref(false);
const searchError = ref('');
let searchTimer: number | undefined;
let searchGeneration = 0;
let searchAbort: AbortController | undefined;
// Search membership is server-owned. Fleet/layout snapshots are replaced after
// polling, SSE invalidations, and mutations; while a query is open each fresh
// snapshot reruns the same guarded search so renamed, moved, created, and
// deleted sessions enter or leave the result set correctly.
function queueSearch(clearResults: boolean) {
  window.clearTimeout(searchTimer);
  searchAbort?.abort();
  const generation = ++searchGeneration;
  if (route.path !== '/') {
    searching.value = false;
    return;
  }
  const query = searchText.value.trim();
  if (clearResults) {
    searchError.value = '';
    if (query) {
      searchResults.value = null;
    }
  }
  if (!query) {
    searchResults.value = null;
    searching.value = false;
    return;
  }
  searching.value = true;
  searchTimer = window.setTimeout(async () => {
    const controller = new AbortController();
    searchAbort = controller;
    try {
      const archivedOnly = view.value === 'history';
      const result = await listSessionSummaries(
        {
          query,
          archived: archivedOnly || includeHistory.value,
          archivedOnly,
          automation: true,
          status: statusFilter.value || undefined,
          attention: attentionFilter.value || undefined,
          creator: creatorFilter.value || undefined,
        },
        controller.signal,
      );
      if (generation === searchGeneration) {
        searchResults.value = result;
        searchError.value = '';
      }
    } catch (cause) {
      if (generation === searchGeneration && (cause as Error).name !== 'AbortError') {
        searchError.value = (cause as Error).message;
      }
    } finally {
      if (generation === searchGeneration) searching.value = false;
    }
  }, 160);
}
watch(
  [
    searchText,
    includeHistory,
    statusFilter,
    attentionFilter,
    creatorFilter,
    () => view.value,
    () => route.path,
  ],
  () => queueSearch(true),
  { flush: 'sync' },
);
watch([sessions, layout], () => queueSearch(false));
onActivated(() => {
  recordSessionListReturn();
  if (view.value === 'history') void loadHistory();
  queueSearch(true);
});

function matchesFilters(session: SessionSummary): boolean {
  if (!matchesCreator(session)) return false;
  if (statusFilter.value && session.status !== statusFilter.value) return false;
  const attention = effectiveAttention(session).level;
  if (attentionFilter.value === 'needs' && attention === 'ok') return false;
  if (attentionFilter.value === 'ok' && attention !== 'ok') return false;
  if (attentionFilter.value === 'blocked' && attention !== 'blocked') return false;
  if (attentionFilter.value === 'attention' && attention !== 'attention') return false;
  return true;
}

const creatorScopedLiveSessions = computed(() => liveSessions.value.filter(matchesCreator));
const creatorScopedHistorySessions = computed(() => historySessions.value.filter(matchesCreator));
const creatorScopedAttentionSessions = computed(() =>
  creatorScopedLiveSessions.value.filter((session) => effectiveAttention(session).level !== 'ok'),
);
function matchesCreator(session: SessionSummary): boolean {
  const mine = session.created_by === me.username;
  const ops = session.class === 'automation';
  const otherUser = !!session.created_by && session.created_by !== me.username;
  return matchesCreatorScope(mine, ops, otherUser);
}
function matchesRunCreator(run: AutomationRun): boolean {
  const mine = run.actor_subject === me.username;
  const otherUser = !!run.actor_subject && run.actor_subject !== me.username;
  return matchesCreatorScope(mine, true, otherUser);
}
function matchesCreatorScope(mine: boolean, ops: boolean, otherUser: boolean): boolean {
  if (creatorFilter.value === 'mine') return mine;
  if (creatorFilter.value === 'ops') return ops;
  if (creatorFilter.value === 'mine-and-ops') return mine || ops;
  if (creatorFilter.value === 'other-users') return otherUser && !ops;
  return true;
}

const baseSessions = computed(() => {
  if (searchResults.value) {
    const current = new Map(sessions.value.map((session) => [session.id, session]));
    return searchResults.value.map((result) => current.get(result.id) ?? result);
  }
  if (view.value === 'history') return creatorScopedHistorySessions.value;
  if (view.value === 'attention') return creatorScopedAttentionSessions.value;
  if (view.value === 'all') return creatorScopedLiveSessions.value;
  const spaceId = activeSpace.value?.id;
  return creatorScopedLiveSessions.value.filter(
    (session) => session.placement?.space_id === spaceId,
  );
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
  }, new Map<string, SessionSummary[]>()),
);
const sessionsById = computed(
  () => new Map(sessions.value.map((session) => [session.id, session])),
);
function parentSessionOf(session: SessionSummary) {
  if (session.parent_session_id) {
    return sessionsById.value.get(session.parent_session_id);
  }
  if (!session.parent_id) return undefined;
  const legacyCandidates = sessionsByBranch.value.get(session.parent_id) ?? [];
  return legacyCandidates.length === 1 ? legacyCandidates[0] : undefined;
}
function sessionName(session: SessionSummary) {
  return session.branch.title || session.branch.name;
}
function directSessionComparison(left: SessionSummary, right: SessionSummary) {
  if (sessionSort.value === 'name') {
    return sessionName(left).localeCompare(sessionName(right), undefined, {
      numeric: true,
      sensitivity: 'base',
    });
  }
  if (sessionSort.value === 'activity') {
    return right.last_activity_at.localeCompare(left.last_activity_at);
  }
  if (sessionSort.value === 'created') {
    return right.created_at.localeCompare(left.created_at);
  }
  return 0;
}
function nameParentOf(session: SessionSummary, input: SessionSummary[]) {
  const path = sessionName(session).split(/\s+\/\s+/);
  if (path.length < 2) return undefined;
  const candidates = input
    .filter((candidate) => candidate.id !== session.id)
    .map((candidate) => ({
      session: candidate,
      path: sessionName(candidate).split(/\s+\/\s+/),
    }))
    .filter(
      (candidate) =>
        candidate.path.length < path.length &&
        candidate.path.every(
          (segment, index) =>
            segment.localeCompare(path[index], undefined, { sensitivity: 'base' }) === 0,
        ),
    );
  const longest = Math.max(0, ...candidates.map((candidate) => candidate.path.length));
  const closest = candidates.filter((candidate) => candidate.path.length === longest);
  return closest.length === 1 ? closest[0].session : undefined;
}
function sessionTree(input: SessionSummary[]): SessionTreeRow[] {
  const inputIndex = new Map(input.map((session, index) => [session.id, index]));
  const inputById = new Map(input.map((session) => [session.id, session]));
  const inputByBranch = input.reduce((byBranch, session) => {
    const candidates = byBranch.get(session.branch.id) ?? [];
    candidates.push(session);
    byBranch.set(session.branch.id, candidates);
    return byBranch;
  }, new Map<string, SessionSummary[]>());
  const inputIds = new Set(inputIndex.keys());
  const children = new Map<string, SessionSummary[]>();
  const roots: SessionSummary[] = [];

  for (const session of input) {
    const legacyParents = session.parent_id ? (inputByBranch.get(session.parent_id) ?? []) : [];
    const persistedParent = session.parent_session_id
      ? inputById.get(session.parent_session_id)
      : legacyParents.length === 1
        ? legacyParents[0]
        : parentSessionOf(session);
    const parent =
      persistedParent ??
      (!session.parent_session_id && !session.parent_id ? nameParentOf(session, input) : undefined);
    if (!parent || parent.id === session.id || !inputIds.has(parent.id)) {
      roots.push(session);
      continue;
    }
    const siblings = children.get(parent.id) ?? [];
    siblings.push(session);
    children.set(parent.id, siblings);
  }

  const threadTimestamp = new Map<string, string>();
  function timestampFor(session: SessionSummary, visiting = new Set<string>()): string {
    const cached = threadTimestamp.get(session.id);
    if (cached !== undefined) return cached;
    if (visiting.has(session.id)) return '';
    visiting.add(session.id);
    const own = sessionSort.value === 'created' ? session.created_at : session.last_activity_at;
    const latest = (children.get(session.id) ?? []).reduce((value, child) => {
      const childTimestamp = timestampFor(child, visiting);
      return childTimestamp > value ? childTimestamp : value;
    }, own);
    visiting.delete(session.id);
    threadTimestamp.set(session.id, latest);
    return latest;
  }
  function compare(left: SessionSummary, right: SessionSummary) {
    if (sessionSort.value === 'activity' || sessionSort.value === 'created') {
      const difference = timestampFor(right).localeCompare(timestampFor(left));
      if (difference) return difference;
    }
    return (
      directSessionComparison(left, right) ||
      (inputIndex.get(left.id) ?? 0) - (inputIndex.get(right.id) ?? 0)
    );
  }

  const rows: SessionTreeRow[] = [];
  const emitted = new Set<string>();
  function append(session: SessionSummary, depth: number) {
    if (emitted.has(session.id)) return;
    emitted.add(session.id);
    rows.push({ session, depth });
    for (const child of [...(children.get(session.id) ?? [])].sort(compare)) {
      append(child, depth + 1);
    }
  }
  for (const root of [...roots].sort(compare)) append(root, 0);
  // Corrupt legacy ancestry can contain a cycle. Keep every row operable by
  // projecting any unvisited nodes as roots rather than dropping the cycle.
  for (const session of [...input].sort(compare)) append(session, 0);
  return rows;
}
const groupedSessions = computed(() =>
  (activeSpace.value?.groups ?? []).map((group) => ({
    group,
    sessions: group.session_ids
      .map((id) => visibleById.value.get(id))
      .filter((session): session is SessionSummary => !!session),
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
const displayedGroups = computed(() => {
  if (view.value === 'space' && searchResults.value === null) {
    return groupedSessions.value.map(({ group, sessions }) => ({
      key: group.id,
      group,
      rows: sessionTree(sessions),
      qualified: false,
    }));
  }
  return smartSessions.value.length
    ? [
        {
          key: 'smart',
          group: undefined,
          rows: sessionTree(smartSessions.value),
          qualified: true,
        },
      ]
    : [];
});
const cursorSessionIds = computed(() =>
  displayedGroups.value.flatMap((display) =>
    display.group?.collapsed ? [] : display.rows.map(({ session }) => session.id),
  ),
);
const cursorSessionId = ref('');
const cursorSession = computed(() =>
  displayedGroups.value
    .flatMap((display) => display.rows.map(({ session }) => session))
    .find((session) => session.id === cursorSessionId.value),
);
const cursorPosition = computed(() => cursorSessionIds.value.indexOf(cursorSessionId.value) + 1);
watch(
  cursorSessionIds,
  (ids) => {
    if (!ids.includes(cursorSessionId.value)) cursorSessionId.value = ids[0] ?? '';
  },
  { immediate: true },
);
watch(cursorSessionId, focusSession, { immediate: true });
const unmatchedRuns = computed(() =>
  unmatchedAutomationRuns(runs.value, sessions.value).sort((left, right) =>
    right.updated_at.localeCompare(left.updated_at),
  ),
);
const interventionRuns = computed(() =>
  unmatchedRuns.value.filter((run) => unmatchedRunProjection(run) === 'intervention'),
);
const provisioningRuns = computed(() =>
  unmatchedRuns.value.filter((run) => unmatchedRunProjection(run) === 'provisioning'),
);
const historicalRuns = computed(() =>
  unmatchedRuns.value.filter((run) => unmatchedRunProjection(run) === 'history'),
);
const visibleInterventionRuns = computed(() => interventionRuns.value.filter(matchesRunCreator));
const visibleProvisioningRuns = computed(() => provisioningRuns.value.filter(matchesRunCreator));
const visibleHistoricalRuns = computed(() => historicalRuns.value.filter(matchesRunCreator));
const showInterventions = computed(
  () =>
    view.value === 'attention' ||
    (view.value === 'space' && activeSpace.value?.system_key === 'ops'),
);
const operationalRuns = computed(() =>
  view.value === 'attention'
    ? visibleInterventionRuns.value
    : [...visibleInterventionRuns.value, ...visibleProvisioningRuns.value].sort((left, right) =>
        right.updated_at.localeCompare(left.updated_at),
      ),
);

const expandedRows = ref(new Set<string>());
const rowDetails = ref<Record<string, Session>>({});
const rowDetailLoading = ref(new Set<string>());
const rowDetailErrors = ref<Record<string, string>>({});
function rowExpanded(id: string) {
  return expandedRows.value.has(id);
}

async function loadRowDetail(id: string) {
  if (rowDetails.value[id] || rowDetailLoading.value.has(id)) return;
  rowDetailLoading.value = new Set(rowDetailLoading.value).add(id);
  const errors = { ...rowDetailErrors.value };
  delete errors[id];
  rowDetailErrors.value = errors;
  try {
    rowDetails.value = { ...rowDetails.value, [id]: await getSession(id) };
  } catch (cause) {
    rowDetailErrors.value = { ...rowDetailErrors.value, [id]: (cause as Error).message };
  } finally {
    const loading = new Set(rowDetailLoading.value);
    loading.delete(id);
    rowDetailLoading.value = loading;
  }
}

const mailboxPreviewVisible = ref(false);
let previewMedia: MediaQueryList | undefined;
let previewTimer: number | undefined;
function syncMailboxPreview() {
  mailboxPreviewVisible.value = previewMedia?.matches ?? false;
}
onMounted(() => {
  previewMedia = window.matchMedia('(min-width: 64rem)');
  syncMailboxPreview();
  previewMedia.addEventListener('change', syncMailboxPreview);
});
watch(
  [cursorSessionId, mailboxPreviewVisible],
  ([id, visible]) => {
    window.clearTimeout(previewTimer);
    if (!visible || !id || rowDetails.value[id] || rowDetailLoading.value.has(id)) return;
    previewTimer = window.setTimeout(() => void loadRowDetail(id), 120);
  },
  { immediate: true },
);
onBeforeUnmount(() => {
  window.clearTimeout(previewTimer);
  previewMedia?.removeEventListener('change', syncMailboxPreview);
});

function toggleRow(id: string) {
  const next = new Set(expandedRows.value);
  if (next.has(id)) next.delete(id);
  else {
    next.add(id);
    void loadRowDetail(id);
  }
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
function selectedInLayoutOrder() {
  const remaining = new Set(selected.value);
  const ordered = allGroups.value.flatMap(({ group }) =>
    group.session_ids.filter((id) => remaining.delete(id)),
  );
  return [...ordered, ...remaining];
}
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
const { busy: layoutBusy, error: layoutError, run: runLayout } = useLayoutCommands(layout, refresh);

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

async function performMove(
  ids: string[],
  destinationGroupId: string,
  beforeSessionId: string | null = null,
  reversible = true,
) {
  if (!ids.length || !layout.value) return;
  const outcome: { undo?: UndoMove } = {};
  const moved = await runLayout((current) => {
    outcome.undo = snapshotUndo(ids, destinationGroupId);
    return moveSessions({
      session_ids: ids,
      destination_group_id: destinationGroupId,
      before_session_id: beforeSessionId,
      expected_revision: current.revision,
    });
  });
  const undo = outcome.undo;
  if (!moved || !undo) return;
  moveOpenId.value = '';
  if (reversible) undoMove.value = undo;
  announcement.value = undo.message;
  clearSelection();
  await nextTick();
  const movedRow =
    ids.length === 1
      ? document.querySelector<HTMLElement>(
          `[data-session-id="${CSS.escape(ids[0])}"] [data-testid="session-details-toggle"]`,
        )
      : null;
  const focusableMovedRow =
    movedRow && movedRow.offsetParent !== null && !movedRow.hasAttribute('disabled')
      ? movedRow
      : null;
  (focusableMovedRow ?? undoButton.value)?.focus();
}

async function restoreMove() {
  const undo = undoMove.value;
  if (!undo) return;
  const restored = await runLayout((current) =>
    restoreSessionGroups({
      groups: undo.groups,
      expected_revision: current.revision,
    }),
  );
  if (restored) {
    undoMove.value = null;
    announcement.value = 'Move undone';
  }
}

const moveOpenId = ref('');
const rowDestination = ref<Record<string, string>>({});
const rowBefore = ref<Record<string, string>>({});
function openMove(session: SessionSummary) {
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
  const collapsed = !group.collapsed;
  await runLayout(() => setSessionGroupPreference(group.id, collapsed));
}

const organizeOpen = ref(false);
const organizeSpaceId = ref('');
const newSpaceName = ref('');
const newGroupName = ref('');
const spaceNames = ref<Record<string, string>>({});
const groupNames = ref<Record<string, string>>({});
const deleteDestinations = ref<Record<string, string>>({});
const groupDestinationSpaces = ref<Record<string, string>>({});
const dirtySpaceNames = new Set<string>();
const dirtyGroupNames = new Set<string>();
const dirtyGroupDestinations = new Set<string>();
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
      if (!dirtySpaceNames.has(space.id)) spaceNames.value[space.id] = space.name;
      for (const group of space.groups) {
        if (!dirtyGroupNames.has(group.id)) groupNames.value[group.id] = group.name;
        if (!dirtyGroupDestinations.has(group.id)) {
          groupDestinationSpaces.value[group.id] = space.id;
        }
      }
    }
  },
  { immediate: true },
);
const organizeSpace = computed(() =>
  layout.value?.spaces.find((space) => space.id === organizeSpaceId.value),
);
const pendingDeleteGroups = computed(() => {
  const pending = pendingDelete.value;
  if (!pending || !layout.value) return [];
  if (pending.kind === 'group') {
    return allGroups.value
      .filter(({ group }) => group.id === pending.item.id)
      .map(({ group }) => group);
  }
  return layout.value.spaces.find((space) => space.id === pending.item.id)?.groups ?? [];
});
const pendingDeleteRequiresDestination = computed(() => {
  const ids = new Set(pendingDeleteGroups.value.map((group) => group.id));
  return (
    pendingDeleteGroups.value.some((group) => group.session_ids.length > 0) ||
    (layout.value?.defaults ?? []).some((fallback) => ids.has(fallback.group_id))
  );
});
const pendingDeleteImpossible = computed(() => {
  const pending = pendingDelete.value;
  if (!pending || !layout.value) return true;
  if (pending.kind === 'space') return layout.value.spaces.length <= 1;
  const space = layout.value.spaces.find((candidate) =>
    candidate.groups.some((group) => group.id === pending.item.id),
  );
  return !space || space.groups.length <= 1;
});
const pendingDeleteDestination = computed(() => {
  const pending = pendingDelete.value;
  if (!pending) return undefined;
  const id = deleteDestinations.value[pending.item.id];
  const sourceIds = new Set(pendingDeleteGroups.value.map((group) => group.id));
  return allGroups.value.find(({ group }) => group.id === id && !sourceIds.has(group.id));
});
const deleteConfirmationDisabled = computed(
  () =>
    layoutBusy.value ||
    pendingDeleteImpossible.value ||
    (pendingDeleteRequiresDestination.value && !pendingDeleteDestination.value),
);
const deleteDescription = computed(() => {
  const pending = pendingDelete.value;
  if (!pending) return '';
  const label = `${pending.kind} “${pending.item.name}”`;
  if (pendingDeleteDestination.value) {
    return `Delete ${label} and move its sessions and placement defaults to “${pendingDeleteDestination.value.label}”.`;
  }
  if (pendingDeleteRequiresDestination.value) {
    return `Select a destination before deleting ${label}; it still owns sessions or placement defaults.`;
  }
  return `Delete empty ${label}.`;
});

function editSpaceName(id: string, name: string) {
  dirtySpaceNames.add(id);
  spaceNames.value[id] = name;
}
function editGroupName(id: string, name: string) {
  dirtyGroupNames.add(id);
  groupNames.value[id] = name;
}
function editGroupDestination(id: string, spaceId: string) {
  dirtyGroupDestinations.add(id);
  groupDestinationSpaces.value[id] = spaceId;
}
function addSpace() {
  if (!layout.value || !newSpaceName.value.trim()) return;
  const name = newSpaceName.value;
  void runLayout((current) => createSessionSpace(name, current.revision)).then((created) => {
    if (created) newSpaceName.value = '';
  });
}
function renameSpace(space: SessionSpace) {
  if (!layout.value) return;
  void runLayout((current) =>
    updateSessionSpace(space.id, spaceNames.value[space.id], current.revision),
  ).then((renamed) => {
    if (renamed) dirtySpaceNames.delete(space.id);
  });
}
function removeSpace(space: SessionSpace) {
  if (!layout.value) return;
  void runLayout((current) =>
    deleteSessionSpace(space.id, deleteDestinations.value[space.id] || null, current.revision),
  );
}
function addGroup(space: SessionSpace) {
  if (!layout.value || !newGroupName.value.trim()) return;
  const name = newGroupName.value;
  void runLayout((current) => createSessionGroup(space.id, name, current.revision)).then(
    (created) => {
      if (created) newGroupName.value = '';
    },
  );
}
function renameGroup(group: SessionGroup) {
  if (!layout.value) return;
  void runLayout((current) =>
    updateSessionGroup(group.id, groupNames.value[group.id], current.revision),
  ).then((renamed) => {
    if (renamed) dirtyGroupNames.delete(group.id);
  });
}
function removeGroup(group: SessionGroup) {
  if (!layout.value) return;
  void runLayout((current) =>
    deleteSessionGroup(group.id, deleteDestinations.value[group.id] || null, current.revision),
  );
}
function confirmLayoutDelete() {
  const pending = pendingDelete.value;
  if (deleteConfirmationDisabled.value) return;
  pendingDelete.value = null;
  if (!pending) return;
  if (pending.kind === 'space') removeSpace(pending.item);
  else removeGroup(pending.item);
}

function groupIsLast(group: SessionGroup) {
  return layout.value?.spaces.find((space) => space.id === group.space_id)?.groups.length === 1;
}
function reorderSpace(space: SessionSpace, direction: -1 | 1) {
  if (!layout.value) return;
  const spaces = layout.value.spaces;
  const index = spaces.findIndex((candidate) => candidate.id === space.id);
  const target = index + direction;
  if (target < 0 || target >= spaces.length) return;
  const beforeId = direction < 0 ? spaces[target].id : spaces[target + 1]?.id;
  void runLayout((current) =>
    reorderSessionLayout({
      kind: 'space',
      id: space.id,
      before_id: beforeId ?? null,
      expected_revision: current.revision,
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
  void runLayout((current) =>
    reorderSessionLayout({
      kind: 'group',
      id: group.id,
      before_id: beforeId ?? null,
      destination_space_id: group.space_id,
      expected_revision: current.revision,
    }),
  );
}
function moveGroupToSpace(group: SessionGroup) {
  if (!layout.value) return;
  const destination = groupDestinationSpaces.value[group.id];
  if (!destination || destination === group.space_id) return;
  void runLayout((current) =>
    reorderSessionLayout({
      kind: 'group',
      id: group.id,
      before_id: null,
      destination_space_id: destination,
      expected_revision: current.revision,
    }),
  ).then((moved) => {
    if (moved) dirtyGroupDestinations.delete(group.id);
  });
}

const pendingBulkArchive = ref(false);
const archiveResult = ref('');
async function confirmBulkArchive() {
  pendingBulkArchive.value = false;
  error.value = '';
  const ids = [...selected.value];
  const results = await Promise.allSettled(ids.map(archiveSession));
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

function focusCursor() {
  if (!cursorSessionId.value) cursorSessionId.value = cursorSessionIds.value[0] ?? '';
  const id = cursorSessionId.value;
  if (!id) return;
  void nextTick(() => {
    document
      .querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"] [data-session-primary]`)
      ?.focus({ preventScroll: true });
    document
      .querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"]`)
      ?.scrollIntoView({ block: 'nearest' });
  });
}

function moveCursor(direction: -1 | 1) {
  const ids = cursorSessionIds.value;
  if (!ids.length) return;
  const current = ids.indexOf(cursorSessionId.value);
  const fallback = direction > 0 ? 0 : ids.length - 1;
  const next = current < 0 ? fallback : Math.min(Math.max(current + direction, 0), ids.length - 1);
  cursorSessionId.value = ids[next];
  focusCursor();
}

function moveCursorTo(edge: 'first' | 'last') {
  const ids = cursorSessionIds.value;
  cursorSessionId.value = edge === 'first' ? (ids[0] ?? '') : (ids.at(-1) ?? '');
  focusCursor();
}

function activateCursor() {
  if (!cursorSessionId.value) return;
  document
    .querySelector<HTMLAnchorElement>(
      `[data-session-id="${CSS.escape(cursorSessionId.value)}"] [data-session-primary]`,
    )
    ?.click();
}

function toggleCursorSelection() {
  const id = cursorSessionId.value;
  const session = sessionsById.value.get(id);
  if (!id || !session || session.status === 'archived') return;
  setSelected(id, !isSelected(id));
}

function toggleCursorDetails() {
  if (cursorSessionId.value) toggleRow(cursorSessionId.value);
}

const sessionCommands = computed<Command[]>(() => [
  {
    id: 'sessions.cursor-down',
    label: 'Move row cursor down',
    keys: ['j'],
    hint: true,
    run: () => moveCursor(1),
  },
  {
    id: 'sessions.cursor-up',
    label: 'Move row cursor up',
    keys: ['k'],
    run: () => moveCursor(-1),
  },
  {
    id: 'sessions.first',
    label: 'Go to first row',
    keys: ['g g'],
    run: () => moveCursorTo('first'),
  },
  {
    id: 'sessions.last',
    label: 'Go to last row',
    keys: ['G'],
    run: () => moveCursorTo('last'),
  },
  {
    id: 'sessions.open',
    label: 'Open current session',
    keys: ['Enter'],
    hint: true,
    enabled: () => !!cursorSessionId.value,
    run: activateCursor,
  },
  {
    id: 'sessions.search',
    label: 'Search sessions',
    keys: ['/'],
    hint: true,
    run: () => searchInput.value?.focus(),
  },
  {
    id: 'sessions.details',
    label: 'Toggle row details',
    keys: ['o'],
    enabled: () => !!cursorSessionId.value,
    run: toggleCursorDetails,
  },
  {
    id: 'sessions.select',
    label: 'Toggle row selection',
    keys: ['Space', 'x'],
    enabled: () => {
      const session = sessionsById.value.get(cursorSessionId.value);
      return !!session && session.status !== 'archived';
    },
    run: toggleCursorSelection,
  },
  {
    id: 'sessions.refresh',
    label: 'Refresh sessions',
    keys: ['r'],
    run: refresh,
  },
]);
useCommandScope('sessions', 'Sessions', sessionCommands, 100);

const clearingTag = ref('');
async function clearTag(sessionId: string, key: string) {
  clearingTag.value = `${sessionId}:${key}`;
  try {
    await clearSessionTag(sessionId, key);
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
  <div class="session-mailbox px-3 py-2">
    <h1 class="sr-only">Sessions</h1>

    <div class="mb-3 flex flex-wrap items-center gap-2">
      <label class="relative min-w-52 flex-1 sm:max-w-md">
        <span class="sr-only">Search sessions</span>
        <input
          ref="searchInput"
          v-model="searchText"
          type="search"
          data-testid="fleet-search"
          placeholder="Search sessions, prompts, repos, issues…"
          class="w-full border border-line bg-input px-2 py-1 text-xs outline-none focus:ring-1 focus:ring-accent"
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
      <select
        v-model="creatorFilter"
        aria-label="Filter by creator"
        data-testid="creator-filter"
        class="rounded border border-line bg-input px-2 py-1.5 text-xs"
        @change="updateFilters"
      >
        <option value="">Everyone</option>
        <option value="mine-and-ops">Mine + Ops</option>
        <option value="mine">Mine</option>
        <option value="ops">Ops</option>
        <option value="other-users">Other users</option>
      </select>
      <select
        v-model="sessionSort"
        aria-label="Sort sessions"
        data-testid="session-sort"
        class="rounded border border-line bg-input px-2 py-1.5 text-xs"
        @change="updateSort"
      >
        <option value="manual">Manual order</option>
        <option value="activity">Last active</option>
        <option value="name">Name</option>
        <option value="created">Date created</option>
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

    <div class="mb-2 flex min-w-0 items-stretch gap-1">
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
        class="flex min-w-0 flex-1 gap-0.5 overflow-x-auto border border-line bg-surface p-0.5"
      >
        <router-link
          :to="{ path: '/', query: viewQuery('attention') }"
          data-testid="attention-view"
          :aria-current="view === 'attention' ? 'page' : undefined"
          :class="view === 'attention' ? 'bg-attn-soft text-attn' : 'text-muted hover:bg-subtle'"
          class="flex shrink-0 items-center gap-1.5 px-2 py-1 text-xs font-medium"
        >
          Attention
          <span class="font-mono text-2xs">{{
            creatorScopedAttentionSessions.length + visibleInterventionRuns.length
          }}</span>
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
          class="flex shrink-0 items-center gap-1.5 px-2 py-1 text-xs font-medium"
        >
          {{ space.name }}
          <span class="font-mono text-2xs">
            {{
              creatorScopedLiveSessions.filter(
                (session) => session.placement?.space_id === space.id,
              ).length
            }}
          </span>
        </router-link>
        <router-link
          :to="{ path: '/', query: viewQuery('all') }"
          data-testid="all-view"
          :aria-current="view === 'all' ? 'page' : undefined"
          :class="view === 'all' ? 'bg-accent text-accent-fg' : 'text-muted hover:bg-subtle'"
          class="flex shrink-0 items-center gap-1.5 px-2 py-1 text-xs font-medium"
        >
          All <span class="font-mono text-2xs">{{ creatorScopedLiveSessions.length }}</span>
        </router-link>
        <router-link
          :to="{ path: '/', query: viewQuery('history') }"
          data-testid="history-view"
          :aria-current="view === 'history' ? 'page' : undefined"
          :class="view === 'history' ? 'bg-accent text-accent-fg' : 'text-muted hover:bg-subtle'"
          class="flex shrink-0 items-center gap-1.5 px-2 py-1 text-xs font-medium"
        >
          History
          <span class="font-mono text-2xs">{{
            creatorScopedHistorySessions.length + visibleHistoricalRuns.length
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
            :value="spaceNames[organizeSpace.id]"
            aria-label="Space name"
            class="rounded bg-input px-2 py-1 text-xs"
            @input="editSpaceName(organizeSpaceId, ($event.target as HTMLInputElement).value)"
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
            :disabled="layout.spaces.length <= 1"
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
              :value="groupNames[group.id]"
              :aria-label="`Name for ${group.name}`"
              class="rounded bg-input px-2 py-1 text-xs"
              @input="editGroupName(group.id, ($event.target as HTMLInputElement).value)"
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
              :value="groupDestinationSpaces[group.id]"
              :aria-label="`Destination space for ${group.name}`"
              class="rounded bg-input px-2 py-1 text-xs"
              @change="editGroupDestination(group.id, ($event.target as HTMLSelectElement).value)"
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
              :disabled="groupIsLast(group)"
              @click="pendingDelete = { kind: 'group', item: group }"
            >
              Delete
            </button>
          </li>
        </ul>
      </div>
    </section>

    <div
      v-if="error || layoutError || searchError"
      role="alert"
      class="mb-3 rounded border border-block-line/40 bg-block-soft px-3 py-2 text-sm text-block"
    >
      {{ error || layoutError || searchError }}
    </div>
    <div
      v-if="Object.keys(resourceErrors).length"
      role="alert"
      class="mb-3 rounded border border-attn-line/40 bg-attn-soft px-3 py-2 text-sm text-attn"
    >
      <span v-for="(message, resource) in resourceErrors" :key="resource" class="mr-3">
        {{ resource }}: {{ message }}
      </span>
    </div>
    <div
      v-if="archiveResult"
      data-testid="archive-result"
      role="status"
      class="mb-3 rounded border border-line bg-surface px-3 py-2 text-sm text-muted"
    >
      {{ archiveResult }}
    </div>

    <div class="session-workbench-grid">
      <div class="min-w-0">
        <div
          v-if="selected.size"
          data-testid="selection-toolbar"
          class="sticky top-2 z-20 mb-3 flex flex-wrap items-center gap-2 border border-accent bg-surface px-3 py-2"
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
            :disabled="layoutBusy"
            @click="performMove(selectedInLayoutOrder(), bulkDestination)"
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
              operationalRuns.length
            }}</span>
          </div>
          <ul
            v-if="operationalRuns.length"
            data-testid="interventions"
            class="rounded border border-line bg-surface"
          >
            <AutomationRunRow
              v-for="run in operationalRuns"
              :key="run.id"
              :run="run"
              :projection="unmatchedRunProjection(run)"
              @changed="refresh"
              @error="error = $event"
            />
          </ul>
          <p v-else class="rounded border border-dashed border-line px-3 py-3 text-sm text-muted">
            No automation runs need intervention.
          </p>
        </section>

        <section
          v-for="display in displayedGroups"
          :key="display.key"
          :data-group-id="display.group?.id"
          :data-testid="display.group ? 'session-group' : undefined"
          class="session-mailbox-group mb-2 border border-line bg-surface"
          :class="
            display.group && dropGroupId === display.group.id && !dropBeforeId
              ? 'ring-1 ring-accent'
              : ''
          "
          @dragover.prevent="display.group && dragOver(display.group.id)"
          @drop.prevent="display.group && drop(display.group.id)"
        >
          <header
            v-if="display.group"
            class="flex min-h-7 items-center gap-2 border-b border-line bg-input/40 px-2 py-1 font-mono"
          >
            <button
              type="button"
              :aria-expanded="!display.group.collapsed"
              :aria-controls="`group-${display.group.id}`"
              :aria-label="`${display.group.collapsed ? 'Expand' : 'Collapse'} ${display.group.name}`"
              class="flex min-w-0 flex-1 items-center gap-2 text-left"
              @click="toggleGroup(display.group)"
            >
              <span class="text-faint" :class="display.group.collapsed ? '' : 'rotate-90'">▸</span>
              <span class="truncate text-sm font-medium text-fg">{{ display.group.name }}</span>
              <span class="font-mono text-2xs text-faint">{{ display.rows.length }}</span>
            </button>
          </header>
          <ul
            v-show="!display.group?.collapsed"
            :id="display.group ? `group-${display.group.id}` : undefined"
            data-testid="session-list"
          >
            <SessionWorkbenchRow
              v-for="row in display.rows"
              :key="row.session.id"
              :session="row.session"
              :tree-depth="row.depth"
              :reorderable="sessionSort === 'manual'"
              :detail="rowDetails[row.session.id]"
              :detail-loading="rowDetailLoading.has(row.session.id)"
              :detail-error="rowDetailErrors[row.session.id]"
              :qualified="display.qualified"
              :selected="isSelected(row.session.id)"
              :expanded="rowExpanded(row.session.id)"
              :move-open="moveOpenId === row.session.id"
              :destination="rowDestination[row.session.id]"
              :before="rowBefore[row.session.id]"
              :all-groups="allGroups"
              :all-sessions="sessions"
              :parent-session="parentSessionOf(row.session)"
              :dragging="draggingId === row.session.id"
              :drop-before="
                dropGroupId === (display.group?.id ?? row.session.placement?.group_id) &&
                dropBeforeId === row.session.id
              "
              :clearing-tag="clearingTag"
              :cursor="cursorSessionId === row.session.id"
              @activate="cursorSessionId = row.session.id"
              @toggle-select="setSelected(row.session.id, $event)"
              @toggle-details="toggleRow(row.session.id)"
              @open-move="openMove(row.session)"
              @update-destination="rowDestination[row.session.id] = $event"
              @update-before="rowBefore[row.session.id] = $event"
              @move="performMove([row.session.id], $event.groupId, $event.beforeId || null)"
              @drag-start="dragStart(row.session.id, $event)"
              @drag-end="clearDrag"
              @drag-over="
                dragOver(display.group?.id ?? row.session.placement?.group_id ?? '', row.session.id)
              "
              @drop="
                drop(display.group?.id ?? row.session.placement?.group_id ?? '', row.session.id)
              "
              @changed="refresh"
              @error="error = $event"
              @clear-tag="clearTag(row.session.id, $event)"
              @record-open="recordSessionLinkOpen($event, row.session.id)"
            />
            <li
              v-if="display.group && !display.rows.length"
              data-testid="empty-group"
              class="px-3 py-5 text-center text-sm text-faint"
            >
              {{
                display.group.session_ids.length
                  ? 'No sessions match the current view or filters.'
                  : 'Empty group — drop sessions here or use Move.'
              }}
            </li>
          </ul>
        </section>

        <section v-if="view === 'history' && visibleHistoricalRuns.length" class="mt-4">
          <h2 class="mb-1.5 text-2xs font-semibold uppercase tracking-wider text-muted">
            Automation run history
          </h2>
          <ul data-testid="automation-run-history" class="rounded border border-line bg-surface">
            <AutomationRunRow
              v-for="run in visibleHistoricalRuns"
              :key="run.id"
              :run="run"
              projection="history"
              @changed="refresh"
              @error="error = $event"
            />
          </ul>
        </section>

        <div
          v-if="
            (view !== 'space' || searchResults !== null) &&
            !smartSessions.length &&
            !(view === 'history' && visibleHistoricalRuns.length) &&
            !(showInterventions && operationalRuns.length)
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
      </div>

      <SessionMailboxPreview
        :session="cursorSession"
        :detail="cursorSession ? rowDetails[cursorSession.id] : undefined"
        :loading="cursorSession ? rowDetailLoading.has(cursorSession.id) : false"
        :error="cursorSession ? rowDetailErrors[cursorSession.id] : ''"
        :position="cursorPosition"
        :total="cursorSessionIds.length"
        :selected="cursorSession ? isSelected(cursorSession.id) : false"
        :expanded="cursorSession ? rowExpanded(cursorSession.id) : false"
        @open="cursorSession && recordSessionLinkOpen($event, cursorSession.id)"
        @toggle-select="toggleCursorSelection"
        @toggle-details="toggleCursorDetails"
      />
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
        :disabled="layoutBusy"
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
      :title="`Delete ${pendingDelete?.kind ?? 'layout item'} “${pendingDelete?.item.name ?? ''}”?`"
      :description="deleteDescription"
      confirm-label="Delete"
      :busy="layoutBusy"
      :confirm-disabled="deleteConfirmationDisabled"
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
