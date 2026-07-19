<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { getChat, resetChat } from '../api';
import type { Session, AcpCommand } from '../types';
import SessionConversation from '../components/SessionConversation.vue';

// The Chat surface: a conversation with the fleet **concierge** — an agent that
// holds the whole fleet in view and that you ask about your other sessions
// ("any stale sessions?", "what needs me?") and steer them through. The
// concierge is a normal loom session (hidden from the fleet list by its kind);
// `GET /api/chat` get-or-creates the singleton and we mount the existing
// drivable conversation against it — its composer already sends turns to the
// live agent and auto-refreshes on each reply.
type LoadState = 'loading' | 'ready' | 'error';
const state = ref<LoadState>('loading');
const session = ref<Session | null>(null);
const errorMsg = ref('');
const localCommands: AcpCommand[] = [
  {
    name: 'clear',
    description: 'Start a fresh concierge conversation',
    input: null,
  },
];

async function load() {
  state.value = 'loading';
  errorMsg.value = '';
  try {
    session.value = await getChat();
    state.value = 'ready';
  } catch (e) {
    errorMsg.value = e instanceof Error ? e.message : String(e);
    state.value = 'error';
  }
}

// Reset the conversation: archive the current concierge (its transcript is kept
// in history) and mount a fresh one. A two-step click guards the teardown — the
// first arms it, the second confirms — so a stray click never wipes the agent's
// context. The new session has a new id, so the keyed conversation remounts clean.
const resetting = ref(false);
const armed = ref(false);
let disarmTimer: ReturnType<typeof setTimeout> | null = null;

function armReset() {
  armed.value = true;
  if (disarmTimer) clearTimeout(disarmTimer);
  disarmTimer = setTimeout(() => (armed.value = false), 4000);
}

async function startFresh() {
  if (resetting.value) return;
  if (disarmTimer) clearTimeout(disarmTimer);
  armed.value = false;
  resetting.value = true;
  errorMsg.value = '';
  try {
    session.value = await resetChat();
    state.value = 'ready';
  } catch (e) {
    errorMsg.value = e instanceof Error ? e.message : String(e);
    state.value = 'error';
  } finally {
    resetting.value = false;
  }
}

async function reset() {
  if (!armed.value) {
    armReset();
    return;
  }
  await startFresh();
}

function handleCommand(name: string) {
  if (name === 'clear') startFresh();
}

onMounted(load);
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col px-5 py-3">
    <header class="mb-3 flex shrink-0 items-start gap-3">
      <div class="min-w-0 flex-1">
        <h1 class="text-sm font-semibold text-fg">Chat</h1>
        <p class="text-xs text-muted">
          Ask the concierge about your fleet — stale sessions, what needs you — and have it act on
          your behalf.
        </p>
      </div>
      <!-- Reset: archive this concierge (its transcript is kept) and start a
           fresh one. Two-step — the first click arms, the second confirms. -->
      <button
        v-if="state === 'ready'"
        type="button"
        class="btn-secondary shrink-0 px-2 py-0.5 text-xs"
        :class="{ 'text-block': armed }"
        :disabled="resetting"
        :title="
          armed
            ? 'Click again to confirm — the current conversation is archived'
            : 'Start a fresh conversation'
        "
        data-testid="chat-reset"
        @click="reset"
      >
        {{ resetting ? 'Starting…' : armed ? 'Confirm new chat' : 'New chat' }}
      </button>
    </header>

    <p v-if="state === 'loading'" class="text-sm text-muted">Waking the concierge…</p>

    <div v-else-if="state === 'error'" class="text-sm">
      <p class="text-block">{{ errorMsg }}</p>
      <button type="button" class="btn-secondary mt-2 px-2 py-0.5 text-xs" @click="load">
        Retry
      </button>
    </div>

    <!-- The concierge is a session, so the standard drivable conversation view
         is the whole chat: skimmable log + a composer that sends to the agent. -->
    <SessionConversation
      v-else-if="session"
      :key="session.id"
      :session="session"
      :local-commands="localCommands"
      class="min-h-0 flex-1"
      @command="handleCommand"
    />
  </div>
</template>
