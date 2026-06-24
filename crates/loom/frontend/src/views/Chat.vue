<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { getChat } from '../api';
import type { Session } from '../types';
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

onMounted(load);
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col px-5 py-3">
    <header class="mb-3 shrink-0">
      <h1 class="text-sm font-semibold text-fg">Chat</h1>
      <p class="text-xs text-muted">
        Ask the concierge about your fleet — stale sessions, what needs you — and
        have it act on your behalf.
      </p>
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
      class="min-h-0 flex-1"
    />
  </div>
</template>
