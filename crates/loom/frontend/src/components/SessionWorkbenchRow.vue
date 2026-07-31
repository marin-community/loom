<script setup lang="ts">
import { computed, nextTick, ref, type PropType, useId, watch } from 'vue';
import type { Session, SessionGroup, SessionSpace, SessionSummary } from '../types';
import AgentUsage from './AgentUsage.vue';
import GithubStatus from './GithubStatus.vue';
import SessionRemedyButton from './SessionRemedyButton.vue';
import SessionRowActions from './SessionRowActions.vue';
import SignalChip from './SignalChip.vue';
import StatusBadge from './StatusBadge.vue';
import TagPill from './TagPill.vue';
import {
  effectiveAttention,
  lifecycleDot,
  messageOf,
  quietTags,
  signalChips,
} from '../lib/sessionState';
import { timeAgo } from '../lib/time';

interface GroupOption {
  group: SessionGroup;
  space: SessionSpace;
  label: string;
}

const props = defineProps({
  session: { type: Object as PropType<SessionSummary>, required: true },
  treeDepth: { type: Number, default: 0 },
  reorderable: { type: Boolean, default: true },
  detail: { type: Object as PropType<Session | undefined>, default: undefined },
  detailLoading: { type: Boolean, default: false },
  detailError: { type: String, default: '' },
  qualified: { type: Boolean, default: false },
  selected: { type: Boolean, default: false },
  expanded: { type: Boolean, default: false },
  moveOpen: { type: Boolean, default: false },
  destination: { type: String, default: '' },
  before: { type: String, default: '' },
  allGroups: { type: Array as PropType<GroupOption[]>, default: () => [] },
  allSessions: { type: Array as PropType<SessionSummary[]>, default: () => [] },
  parentSession: { type: Object as PropType<SessionSummary | undefined>, default: undefined },
  dragging: { type: Boolean, default: false },
  dropBefore: { type: Boolean, default: false },
  clearingTag: { type: String, default: '' },
  cursor: { type: Boolean, default: false },
});

const emit = defineEmits<{
  toggleSelect: [selected: boolean];
  toggleDetails: [];
  openMove: [];
  updateDestination: [groupId: string];
  updateBefore: [sessionId: string];
  move: [position: { groupId: string; beforeId: string }];
  dragStart: [event: DragEvent];
  dragEnd: [];
  dragOver: [];
  drop: [];
  changed: [];
  error: [message: string];
  clearTag: [key: string];
  recordOpen: [event: MouseEvent];
  activate: [];
}>();
const disclosureId = useId();
const detailsButtonId = `${disclosureId}-details-button`;
const detailsId = `${disclosureId}-details`;
const moveId = `${disclosureId}-move`;
const detailsButton = ref<HTMLButtonElement>();
const detailsPanel = ref<HTMLElement>();
const moveButton = ref<HTMLButtonElement>();
const destinationSelect = ref<HTMLSelectElement>();

function title() {
  const task = props.session.branch.title || props.session.branch.name;
  if (!props.qualified || !props.session.placement) return task;
  return `${props.session.placement.group_name} / ${task}`;
}
const titleParts = computed(() =>
  title()
    .split(/\s+\/\s+/)
    .filter((part) => part.length > 0),
);

const positionOptions = computed(() => {
  const group = props.allGroups.find((entry) => entry.group.id === props.destination)?.group;
  if (!group) return [];
  const byId = new Map(props.allSessions.map((session) => [session.id, session]));
  return group.session_ids
    .filter((id) => id !== props.session.id && byId.get(id)?.status !== 'archived')
    .map((id) => ({
      id,
      label: byId.get(id)?.branch.title || byId.get(id)?.branch.name || id,
    }));
});

watch(
  () => props.expanded,
  async (open, wasOpen) => {
    await nextTick();
    if (open) detailsPanel.value?.focus();
    else if (wasOpen) detailsButton.value?.focus();
  },
);
watch(
  () => props.moveOpen,
  async (open, wasOpen) => {
    await nextTick();
    if (open) destinationSelect.value?.focus();
    else if (wasOpen) moveButton.value?.focus();
  },
);

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape') return;
  if (props.moveOpen) {
    event.preventDefault();
    emit('openMove');
  } else if (props.expanded) {
    event.preventDefault();
    emit('toggleDetails');
  }
}
</script>

<template>
  <li
    data-testid="session-card"
    :data-session-id="session.id"
    :data-tree-depth="treeDepth"
    class="session-mailbox-row group relative flex flex-wrap items-start gap-1.5 border-b border-line px-2 py-1.5 last:border-0 hover:bg-subtle/70 focus-within:bg-subtle/70"
    :class="[
      dragging ? 'opacity-40' : '',
      dropBefore ? 'shadow-[inset_0_2px_0_var(--accent)]' : '',
      cursor ? 'session-mailbox-row--cursor' : '',
    ]"
    :data-cursor="cursor ? 'true' : undefined"
    :style="{ paddingLeft: `${0.5 + Math.min(treeDepth, 5)}rem` }"
    @dragover.stop.prevent="emit('dragOver')"
    @drop.stop.prevent="emit('drop')"
    @keydown="onKeydown"
    @focusin="emit('activate')"
    @mousedown="emit('activate')"
  >
    <span
      v-if="session.status !== 'archived' && reorderable"
      draggable="true"
      data-testid="session-drag"
      aria-hidden="true"
      class="relative z-10 mt-0.5 cursor-grab text-faint opacity-50 hover:text-fg group-hover:opacity-100"
      @dragstart="emit('dragStart', $event)"
      @dragend="emit('dragEnd')"
    >
      ⠿
    </span>
    <span v-else class="w-3" aria-hidden="true"></span>
    <span v-if="treeDepth" class="mt-0.5 shrink-0 font-mono text-xs text-faint" aria-hidden="true">
      └
    </span>

    <input
      v-if="session.status !== 'archived'"
      type="checkbox"
      :checked="selected"
      :aria-label="`Select ${session.branch.title || session.branch.name}`"
      class="relative z-10 mt-1"
      @click.stop="emit('toggleSelect', ($event.target as HTMLInputElement).checked)"
    />

    <span
      class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
      :class="lifecycleDot(session)"
      :title="`${session.status}; ${effectiveAttention(session).level}`"
      aria-hidden="true"
    ></span>

    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-2">
        <router-link
          :to="`/s/${session.id}`"
          data-session-primary
          :tabindex="cursor ? 0 : -1"
          class="session-mailbox-primary stretched-link min-w-0 truncate font-mono text-xs font-medium leading-4 text-fg hover:text-accent"
          @click="emit('recordOpen', $event)"
        >
          <template v-for="(part, index) in titleParts" :key="`${part}-${index}`">
            <span v-if="index" class="text-faint"> / </span>
            <span :class="index < titleParts.length - 1 ? 'text-muted' : ''">{{ part }}</span>
          </template>
        </router-link>
        <span
          v-if="effectiveAttention(session).level !== 'ok'"
          class="shrink-0 rounded px-1.5 py-0.5 text-2xs font-medium"
          :class="effectiveAttention(session).level === 'blocked' ? 'text-block' : 'text-attn-line'"
        >
          {{ effectiveAttention(session).level }}
        </span>
        <span
          v-if="session.profile && session.profile !== 'default'"
          class="meta-chip shrink-0"
          :title="`profile: ${session.profile}`"
        >
          {{ session.profile }}
        </span>
        <span
          v-if="['created', 'done', 'error', 'orphaned'].includes(session.status)"
          class="meta-chip shrink-0"
          :aria-label="`Lifecycle: ${session.status}`"
        >
          {{ session.status }}
        </span>
      </div>
      <p
        v-if="messageOf(session)"
        class="mt-0.5 truncate text-xs leading-4 text-muted"
        data-testid="session-status-message"
      >
        {{ messageOf(session) }}
      </p>
    </div>

    <time
      v-if="session.last_activity_at"
      :datetime="session.last_activity_at"
      class="shrink-0 font-mono text-2xs text-faint"
    >
      {{ timeAgo(session.last_activity_at) }}
    </time>

    <SessionRemedyButton :ws="session" @changed="emit('changed')" @error="emit('error', $event)" />
    <button
      ref="detailsButton"
      :id="detailsButtonId"
      type="button"
      data-testid="session-details-toggle"
      class="relative z-10 rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-input hover:text-fg"
      :aria-expanded="expanded"
      :aria-controls="detailsId"
      @click.stop="emit('toggleDetails')"
    >
      Details
    </button>
    <SessionRowActions :ws="session" @changed="emit('changed')" @error="emit('error', $event)" />

    <div
      v-show="expanded"
      :id="detailsId"
      ref="detailsPanel"
      data-testid="session-preview"
      role="region"
      :aria-labelledby="detailsButtonId"
      tabindex="-1"
      class="relative z-10 ml-8 basis-full rounded border border-line bg-canvas px-3 py-2 text-xs outline-none"
    >
      <div class="flex flex-wrap items-center gap-2">
        <StatusBadge v-if="session.status !== 'running'" :status="session.status" />
        <SignalChip
          v-for="chip in signalChips(session)"
          :key="chip.key"
          :chip="chip"
          :busy="clearingTag === `${session.id}:${chip.key}`"
          @clear="emit('clearTag', $event)"
        />
        <TagPill
          v-for="tag in quietTags(session)"
          :key="tag.key"
          :tag="tag"
          :busy="clearingTag === `${session.id}:${tag.key}`"
          @clear="emit('clearTag', $event)"
        />
        <AgentUsage v-if="session.usage" :usage="session.usage" compact />
        <span v-if="session.origin !== 'user'" class="tag-pill">origin: {{ session.origin }}</span>
      </div>
      <p v-if="detail?.branch.goal" class="mt-1 line-clamp-2 text-muted">
        {{ detail.branch.goal }}
      </p>
      <p v-else-if="detailLoading" class="mt-1 text-faint">Loading details…</p>
      <p v-else-if="detailError" class="mt-1 text-block" role="alert">{{ detailError }}</p>
      <div class="mt-1 flex flex-wrap gap-x-3 gap-y-1 font-mono text-2xs text-faint">
        <span>{{ session.branch.repo_root }}</span>
        <span>{{ session.branch.branch }}</span>
        <span v-if="session.created_by">launched by {{ session.created_by }}</span>
        <router-link
          v-if="parentSession"
          :to="`/s/${parentSession.id}`"
          class="relative z-10 text-accent hover:underline"
        >
          delegated from {{ parentSession.branch.title || parentSession.branch.name }}
        </router-link>
      </div>
      <GithubStatus v-if="session.branch.github" :gh="session.branch.github" compact class="mt-1" />

      <div v-if="session.status !== 'archived'" class="mt-2 border-t border-line pt-2">
        <button
          ref="moveButton"
          type="button"
          data-testid="move-session"
          class="rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-input hover:text-fg"
          :aria-expanded="moveOpen"
          :aria-controls="moveId"
          @click.stop="emit('openMove')"
        >
          Move…
        </button>
        <div
          v-if="moveOpen"
          :id="moveId"
          data-testid="move-session-panel"
          class="mt-2 flex flex-wrap items-center gap-2 rounded border border-line bg-input px-2 py-2"
        >
          <label class="text-xs text-muted">
            Move to
            <select
              ref="destinationSelect"
              :value="destination"
              class="ml-1 rounded border border-line bg-surface px-2 py-1 text-xs"
              @change="
                emit('updateDestination', ($event.target as HTMLSelectElement).value);
                emit('updateBefore', '');
              "
            >
              <option v-for="entry in allGroups" :key="entry.group.id" :value="entry.group.id">
                {{ entry.label }}
              </option>
            </select>
          </label>
          <label class="text-xs text-muted">
            Position
            <select
              :value="before"
              class="ml-1 max-w-64 rounded border border-line bg-surface px-2 py-1 text-xs"
              @change="emit('updateBefore', ($event.target as HTMLSelectElement).value)"
            >
              <option value="">At end</option>
              <option v-for="option in positionOptions" :key="option.id" :value="option.id">
                Before {{ option.label }}
              </option>
            </select>
          </label>
          <button
            type="button"
            class="btn-primary px-2 py-1 text-xs"
            :disabled="!destination"
            @click="emit('move', { groupId: destination, beforeId: before })"
          >
            Move
          </button>
        </div>
      </div>
    </div>
  </li>
</template>
