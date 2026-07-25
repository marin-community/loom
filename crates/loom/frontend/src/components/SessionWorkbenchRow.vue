<script setup lang="ts">
import type { PropType } from 'vue';
import type { Session, SessionGroup, SessionSpace } from '../types';
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
  session: { type: Object as PropType<Session>, required: true },
  qualified: { type: Boolean, default: false },
  selected: { type: Boolean, default: false },
  expanded: { type: Boolean, default: false },
  moveOpen: { type: Boolean, default: false },
  destination: { type: String, default: '' },
  allGroups: { type: Array as PropType<GroupOption[]>, default: () => [] },
  dragging: { type: Boolean, default: false },
  dropBefore: { type: Boolean, default: false },
  clearingTag: { type: String, default: '' },
});

const emit = defineEmits<{
  toggleSelect: [];
  toggleDetails: [];
  openMove: [];
  updateDestination: [groupId: string];
  move: [groupId: string];
  dragStart: [event: DragEvent];
  dragEnd: [];
  dragOver: [];
  drop: [];
  changed: [];
  error: [message: string];
  clearTag: [key: string];
  recordOpen: [event: MouseEvent];
}>();

function title() {
  const task = props.session.branch.title || props.session.branch.name;
  if (!props.qualified || !props.session.placement) return task;
  return `${props.session.placement.space_name} / ${props.session.placement.group_name} / ${task}`;
}
</script>

<template>
  <li
    data-testid="session-card"
    :data-session-id="session.id"
    class="group relative flex flex-wrap items-start gap-2 border-b border-line px-3 py-2 last:border-0 hover:bg-subtle focus-within:bg-subtle"
    :class="[
      dragging ? 'opacity-40' : '',
      dropBefore ? 'shadow-[inset_0_2px_0_var(--accent)]' : '',
    ]"
    @dragover.stop.prevent="emit('dragOver')"
    @drop.stop.prevent="emit('drop')"
  >
    <button
      v-if="session.status !== 'archived'"
      type="button"
      draggable="true"
      data-testid="session-drag"
      :aria-label="`Move ${session.branch.title || session.branch.name}`"
      class="relative z-10 mt-0.5 cursor-grab text-faint opacity-50 hover:text-fg focus-visible:opacity-100 group-hover:opacity-100"
      @dragstart="emit('dragStart', $event)"
      @dragend="emit('dragEnd')"
      @click.prevent
    >
      ⠿
    </button>
    <span v-else class="w-3" aria-hidden="true"></span>

    <input
      v-if="session.status !== 'archived'"
      type="checkbox"
      :checked="selected"
      :aria-label="`Select ${session.branch.title || session.branch.name}`"
      class="relative z-10 mt-1"
      @change="emit('toggleSelect')"
      @click.stop
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
          class="stretched-link min-w-0 truncate text-sm font-semibold text-fg hover:text-accent"
          @click="emit('recordOpen', $event)"
        >
          {{ title() }}
        </router-link>
        <span
          v-if="effectiveAttention(session).level !== 'ok'"
          class="shrink-0 text-2xs font-semibold uppercase tracking-wide"
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
      </div>
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
      type="button"
      data-testid="move-session"
      class="relative z-10 rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-input hover:text-fg"
      :aria-expanded="moveOpen"
      @click.stop="emit('openMove')"
    >
      Move…
    </button>
    <button
      type="button"
      data-testid="session-details-toggle"
      class="relative z-10 rounded px-1.5 py-0.5 text-2xs text-muted hover:bg-input hover:text-fg"
      :aria-expanded="expanded"
      @click.stop="emit('toggleDetails')"
    >
      Details
    </button>
    <SessionRowActions :ws="session" @changed="emit('changed')" @error="emit('error', $event)" />

    <div
      v-if="moveOpen"
      data-testid="move-session-panel"
      class="relative z-10 ml-8 flex basis-full flex-wrap items-center gap-2 rounded border border-line bg-input px-2 py-2"
    >
      <label class="text-xs text-muted">
        Move to
        <select
          :value="destination"
          class="ml-1 rounded border border-line bg-surface px-2 py-1 text-xs"
          @change="emit('updateDestination', ($event.target as HTMLSelectElement).value)"
        >
          <option v-for="entry in allGroups" :key="entry.group.id" :value="entry.group.id">
            {{ entry.label }}
          </option>
        </select>
      </label>
      <button
        type="button"
        class="btn-primary px-2 py-1 text-xs"
        :disabled="!destination"
        @click="emit('move', destination)"
      >
        Move
      </button>
    </div>

    <div
      data-testid="session-preview"
      class="relative z-10 ml-8 basis-full rounded border border-line bg-canvas px-3 py-2 text-xs"
      :class="expanded ? 'block' : 'hidden group-hover:block group-focus-within:block'"
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
      <p v-if="messageOf(session)" class="mt-1 text-muted">{{ messageOf(session) }}</p>
      <p v-if="session.branch.goal" class="mt-1 line-clamp-2 text-muted">
        {{ session.branch.goal }}
      </p>
      <div class="mt-1 flex flex-wrap gap-x-3 gap-y-1 font-mono text-2xs text-faint">
        <span>{{ session.branch.repo_root }}</span>
        <span>{{ session.branch.branch }}</span>
        <span v-if="session.created_by">launched by {{ session.created_by }}</span>
        <span v-if="session.parent_id">delegated from {{ session.parent_id }}</span>
      </div>
      <GithubStatus v-if="session.branch.github" :gh="session.branch.github" compact class="mt-1" />
    </div>
  </li>
</template>
