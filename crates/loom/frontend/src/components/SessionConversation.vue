<script setup lang="ts">
import { computed, onActivated, onDeactivated, ref, watch } from 'vue';
import { ensureResumptionCue, getResumptionCue } from '../api';
import { exactTime, timeAgo } from '../lib/time';
import type { Session, AcpCommand, ResumptionCue } from '../types';
import TerminalConversation from './TerminalConversation.vue';
import AcpConversation from './AcpConversation.vue';

// The Conversation surface picks its data source by the session's execution
// backend. An ACP session (`protocol='acp'`) renders from the live chat journal
// (`sessions.chat` + `sessions.chat.stream`); a terminal session keeps the iris scrape path
// (`/conversation`) untouched. One prop, one seam — everything backend-specific
// lives in the two child components.
const props = withDefaults(
  defineProps<{ session: Session; localCommands?: AcpCommand[]; active?: boolean }>(),
  {
    localCommands: () => [],
    active: true,
  },
);
const emit = defineEmits<{ command: [name: string, args: string] }>();
const isAcp = computed(() => props.session.protocol === 'acp');
const forwardCommand = (name: string, args: string) => emit('command', name, args);

const cue = ref<ResumptionCue | null>(null);
const routeActive = ref(true);

onActivated(() => (routeActive.value = true));
onDeactivated(() => (routeActive.value = false));

watch(
  [() => props.session.id, () => props.active && routeActive.value],
  async ([sessionId, active], _, onCleanup) => {
    cue.value = null;
    if (!active) return;

    let cancelled = false;
    onCleanup(() => (cancelled = true));
    try {
      let current = await getResumptionCue(sessionId);
      if (cancelled) return;
      if (current.status === 'due') current = await ensureResumptionCue(sessionId);

      for (
        let attempt = 0;
        !cancelled && current.status === 'generating' && attempt < 24;
        attempt++
      ) {
        await new Promise<void>((resolve) => window.setTimeout(resolve, 2_000));
        if (cancelled) return;
        current = await getResumptionCue(sessionId);
      }
      if (!cancelled && current.status === 'generated') cue.value = current;
    } catch {
      // The update is optional. A failed request should leave the conversation usable.
    }
  },
  { immediate: true },
);
</script>

<template>
  <div class="flex h-full min-h-0 flex-1 flex-col">
    <section
      v-if="cue?.text"
      aria-label="Session update"
      class="mx-3 mt-2 flex shrink-0 flex-wrap items-baseline gap-x-2 gap-y-0.5 border-b border-line px-1 pb-2 text-xs"
    >
      <span class="font-medium text-fg">Session update</span>
      <p class="min-w-0 break-words text-muted">{{ cue.text }}</p>
      <time
        v-if="cue.generated_at"
        :datetime="cue.generated_at"
        :title="exactTime(cue.generated_at)"
        class="text-2xs text-faint"
      >
        {{ timeAgo(cue.generated_at) }}
      </time>
    </section>
    <AcpConversation
      v-if="isAcp"
      class="min-h-0 flex-1"
      :session="session"
      :local-commands="localCommands"
      :active="active"
      @command="forwardCommand"
    />
    <TerminalConversation v-else class="min-h-0 flex-1" :session="session" :active="active" />
  </div>
</template>
