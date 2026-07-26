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
const cueOperation = ref<'' | 'checking' | 'generating'>('');
const cueOpen = ref(false);
const cueError = ref('');
let cueEpoch = 0;

function isCurrent(sessionId: string, epoch: number): boolean {
  return props.session.id === sessionId && cueEpoch === epoch;
}

const wait = (milliseconds: number) =>
  new Promise<void>((resolve) => window.setTimeout(resolve, milliseconds));

async function followGeneration(sessionId: string, epoch: number) {
  for (let attempt = 0; attempt < 24; attempt += 1) {
    await wait(2_000);
    if (!isCurrent(sessionId, epoch)) return;
    const current = await getResumptionCue(sessionId);
    if (!isCurrent(sessionId, epoch)) return;
    cue.value = current;
    if (current.status === 'generated') {
      cueOpen.value = true;
      return;
    }
    if (current.status !== 'generating') return;
  }
}

async function loadCue(onReturn = false) {
  if (cueOperation.value) return;
  const sessionId = props.session.id;
  const epoch = ++cueEpoch;
  cueOperation.value = 'checking';
  cueError.value = '';
  try {
    let current = await getResumptionCue(sessionId);
    let ensuredDue = false;
    if (!isCurrent(sessionId, epoch)) return;
    if (onReturn && current.status === 'due') {
      ensuredDue = true;
      cueOperation.value = 'generating';
      current = await ensureResumptionCue(sessionId);
      if (!isCurrent(sessionId, epoch)) return;
    }
    cue.value = current;
    if (ensuredDue && current.status === 'generated') cueOpen.value = true;
    if (current.status === 'generating') await followGeneration(sessionId, epoch);
  } catch (error) {
    if (isCurrent(sessionId, epoch)) cueError.value = (error as Error).message;
  } finally {
    if (isCurrent(sessionId, epoch)) cueOperation.value = '';
  }
}

async function generateCue() {
  if (cueOperation.value) return;
  const sessionId = props.session.id;
  const epoch = ++cueEpoch;
  cueOperation.value = 'generating';
  cueError.value = '';
  try {
    const generated = await ensureResumptionCue(sessionId, true);
    if (!isCurrent(sessionId, epoch)) return;
    cue.value = generated;
    if (generated.status === 'generated') cueOpen.value = true;
    if (generated.status === 'generating') await followGeneration(sessionId, epoch);
  } catch (error) {
    if (isCurrent(sessionId, epoch)) cueError.value = (error as Error).message;
  } finally {
    if (isCurrent(sessionId, epoch)) cueOperation.value = '';
  }
}

onMounted(() => loadCue(true));
onActivated(() => loadCue(true));
watch(
  () => props.session.id,
  () => {
    cueEpoch += 1;
    cue.value = null;
    cueOpen.value = false;
    cueOperation.value = '';
    cueError.value = '';
    void loadCue(true);
  },
);
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col">
    <section
      v-if="cue?.status === 'generated' && cue.text"
      data-testid="resumption-cue"
      class="mx-3 mt-2 flex items-start gap-2 rounded border border-line bg-subtle px-3 py-2 text-sm"
      aria-label="Generated resumption cue"
    >
      <details
        class="min-w-0 flex-1"
        :open="cueOpen"
        @toggle="cueOpen = ($event.target as HTMLDetailsElement).open"
      >
        <summary class="cursor-pointer text-xs font-medium text-fg">
          Resume context
          <span v-if="cue.generated_at" class="ml-1 font-mono text-2xs font-normal text-faint">
            {{ new Date(cue.generated_at).toLocaleString() }}
          </span>
        </summary>
        <p class="mt-1 whitespace-pre-wrap text-muted">{{ cue.text }}</p>
        <nav v-if="cue.evidence.length" class="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs">
          <router-link
            v-for="evidence in cue.evidence"
            :key="evidence.cursor"
            :to="evidence.href"
            class="text-accent hover:underline"
            >{{ evidence.label }}</router-link
          >
        </nav>
      </details>
      <button
        type="button"
        class="shrink-0 text-xs text-accent hover:underline disabled:opacity-60"
        :disabled="!!cueOperation"
        @click="generateCue"
      >
        {{ cueOperation === 'generating' ? 'Generating…' : 'Refresh' }}
      </button>
    </section>
    <div
      v-else-if="cue?.status !== 'disabled'"
      class="mx-3 mt-1 flex items-center gap-2 text-2xs text-faint"
      role="status"
    >
      <button
        v-if="cue?.status !== 'unavailable'"
        type="button"
        data-testid="generate-resumption-cue"
        class="text-accent hover:underline disabled:opacity-60"
        :disabled="!!cueOperation"
        @click="cue?.status === 'generating' ? loadCue() : generateCue()"
      >
        {{
          cueOperation === 'checking'
            ? 'Checking resumption cue…'
            : cueOperation === 'generating'
              ? 'Generating resumption cue…'
              : cue?.status === 'generating'
                ? 'Check resumption cue'
                : 'Generate resumption cue'
        }}
      </button>
      <span v-else>No eligible metadata profile is available.</span>
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
