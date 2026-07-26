<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from 'vue';
import { ensureResumptionCue, getResumptionCue } from '../api';
import type { Session, AcpCommand, ResumptionCue } from '../types';
import TerminalConversation from './TerminalConversation.vue';
import AcpConversation from './AcpConversation.vue';

// The Conversation surface picks its data source by the session's execution
// backend. An ACP session (`protocol='acp'`) renders from the live chat journal
// (`/chat` + `/chat/stream`); a terminal session keeps the iris scrape path
// (`/conversation`) untouched. One prop, one seam — everything backend-specific
// lives in the two child components.
const props = withDefaults(defineProps<{ session: Session; localCommands?: AcpCommand[] }>(), {
  localCommands: () => [],
});
const emit = defineEmits<{ command: [name: string, args: string] }>();
const isAcp = computed(() => props.session.protocol === 'acp');
const forwardCommand = (name: string, args: string) => emit('command', name, args);

const cue = ref<ResumptionCue | null>(null);
const cueBusy = ref(false);
const cueError = ref('');

async function loadCue(onReturn = false) {
  if (cueBusy.value) return;
  cueBusy.value = true;
  cueError.value = '';
  try {
    let current = await getResumptionCue(props.session.id);
    if (onReturn && current.status === 'due') {
      current = await ensureResumptionCue(props.session.id);
    }
    cue.value = current;
  } catch (error) {
    cueError.value = (error as Error).message;
  } finally {
    cueBusy.value = false;
  }
}

async function generateCue() {
  if (cueBusy.value) return;
  cueBusy.value = true;
  cueError.value = '';
  try {
    cue.value = await ensureResumptionCue(props.session.id, true);
  } catch (error) {
    cueError.value = (error as Error).message;
  } finally {
    cueBusy.value = false;
  }
}

onMounted(() => loadCue(true));
onActivated(() => loadCue(true));
watch(
  () => props.session.id,
  () => {
    cue.value = null;
    void loadCue(true);
  },
);
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <section
      v-if="cue?.status === 'generated' && cue.text"
      data-testid="resumption-cue"
      class="mx-3 mt-2 rounded border border-line bg-subtle px-3 py-2 text-sm"
      aria-label="Generated resumption cue"
    >
      <div class="mb-1 flex flex-wrap items-center gap-2">
        <strong class="text-xs">Generated resumption cue</strong>
        <span v-if="cue.generated_at" class="font-mono text-2xs text-faint">
          {{ new Date(cue.generated_at).toLocaleString() }}
        </span>
        <button
          type="button"
          class="ml-auto text-xs text-accent hover:underline disabled:opacity-60"
          :disabled="cueBusy"
          @click="generateCue"
        >
          {{ cueBusy ? 'Generating…' : 'Refresh' }}
        </button>
      </div>
      <p class="whitespace-pre-wrap text-muted">{{ cue.text }}</p>
      <nav v-if="cue.evidence.length" class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs">
        <router-link
          v-for="evidence in cue.evidence"
          :key="evidence.cursor"
          :to="evidence.href"
          class="text-accent hover:underline"
          >{{ evidence.label }}</router-link
        >
      </nav>
    </section>
    <div
      v-else-if="cue?.status !== 'disabled'"
      class="mx-3 mt-1 flex items-center gap-2 text-2xs text-faint"
      role="status"
    >
      <span v-if="cueBusy || cue?.status === 'generating'">Generating resumption cue…</span>
      <button
        v-else-if="cue?.status !== 'unavailable'"
        type="button"
        data-testid="generate-resumption-cue"
        class="text-accent hover:underline"
        @click="generateCue"
      >
        Generate resumption cue
      </button>
      <span v-if="cueError" class="text-block" role="alert">{{ cueError }}</span>
    </div>
    <AcpConversation
      v-if="isAcp"
      :session="session"
      :local-commands="localCommands"
      @command="forwardCommand"
    />
    <TerminalConversation v-else :session="session" />
  </div>
</template>
