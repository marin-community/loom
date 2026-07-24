<script setup lang="ts">
import { computed } from 'vue';
import type { Session } from '../types';
import { exactTime, timeAgo } from '../lib/time';
import { messageOf, signalChips } from '../lib/sessionState';
import SignalChip from './SignalChip.vue';
import StatusBadge from './StatusBadge.vue';
import SessionRowActions from './SessionRowActions.vue';

const props = defineProps<{
  session: Session;
  parent?: Session;
  tone: 'intervention' | 'active' | 'history';
  clearingTag: string;
}>();

const emit = defineEmits<{ clearTag: [key: string]; changed: []; error: [message: string] }>();

const dotClass = computed(() => {
  if (props.tone === 'intervention') return 'bg-block-line';
  if (props.tone === 'active') return 'bg-ok-line';
  return 'bg-line';
});
</script>

<template>
  <li
    class="group relative flex items-start gap-3 border-b border-line px-3 py-2.5 last:border-0"
    :class="tone === 'history' && 'opacity-70 hover:opacity-100'"
    data-testid="automation-session"
    :data-session-id="session.id"
  >
    <span class="mt-1.5 h-2 w-2 shrink-0 rounded-full" :class="dotClass" aria-hidden="true"></span>
    <div class="min-w-0 flex-1">
      <div class="flex flex-wrap items-center gap-2">
        <router-link
          :to="`/s/${session.id}`"
          class="stretched-link truncate font-serif text-[15px] text-fg hover:text-accent"
          :class="tone === 'history' ? 'font-medium' : 'font-semibold'"
        >
          {{ session.branch.title || session.branch.name }}
        </router-link>
        <StatusBadge :status="session.status" />
        <SignalChip
          v-for="chip in tone === 'intervention' ? signalChips(session) : []"
          :key="chip.key"
          class="relative z-10"
          :chip="chip"
          :busy="clearingTag === `${session.id}:${chip.key}`"
          @clear="(key) => emit('clearTag', key)"
        />
      </div>
      <p
        v-if="tone !== 'history' && messageOf(session)"
        class="mt-0.5 truncate font-serif text-[13px] text-muted"
      >
        {{ messageOf(session) }}
      </p>
      <p v-if="parent" class="mt-0.5 text-xs text-faint">
        launched by
        <router-link class="relative z-10 font-mono hover:text-accent" :to="`/s/${parent.id}`">
          {{ parent.branch.title || parent.branch.name }}
        </router-link>
      </p>
    </div>
    <div class="shrink-0 text-right font-mono text-2xs text-faint">
      <div>{{ session.origin }} · {{ session.profile }}@{{ session.profile_revision }}</div>
      <div>{{ session.turn_count }} turn{{ session.turn_count === 1 ? '' : 's' }}</div>
      <time
        v-if="session.last_activity_at"
        :datetime="session.last_activity_at"
        :title="exactTime(session.last_activity_at)"
        :aria-label="exactTime(session.last_activity_at)"
      >
        {{ timeAgo(session.last_activity_at) }}
      </time>
    </div>
    <SessionRowActions
      :ws="session"
      @changed="emit('changed')"
      @error="(message) => emit('error', message)"
    />
  </li>
</template>
