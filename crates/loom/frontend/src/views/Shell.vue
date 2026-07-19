<script setup lang="ts">
import { ref } from 'vue';
import AgentTerminal from '../components/AgentTerminal.vue';
import { restartShell } from '../api';

// The operator scratch shell: one persistent login shell running inside the
// container, attached over the same terminal bridge agent sessions use. It's
// for one-time setup that would otherwise need `docker exec` — most usefully
// `gcloud auth login`, whose credentials persist in the gcloud config volume.
//
// AgentTerminal is keyed so "Restart" (which kills + respawns the supervisor)
// forces a fresh socket: bumping the key remounts the component, reconnecting
// to the new shell rather than the dead one.
const epoch = ref(0);
const busy = ref(false);
const error = ref('');

async function restart() {
  busy.value = true;
  error.value = '';
  try {
    await restartShell();
    // Give the old supervisor a beat to die and the new one to bind before we
    // reattach; the server already waited, this just covers the socket.
    epoch.value += 1;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="flex min-h-[28rem] flex-1 flex-col px-5 py-3">
    <header class="mb-3 flex items-center justify-between gap-3">
      <div class="min-w-0">
        <h1 class="text-sm font-semibold text-fg">Shell</h1>
        <p class="text-xs text-muted">
          A login shell inside the container — for one-time setup like
          <code class="rounded bg-code px-1 py-0.5 text-[11px]">gcloud auth login</code>. It
          persists across restarts.
        </p>
      </div>
      <button
        type="button"
        class="shrink-0 rounded border border-line px-2.5 py-1 text-xs text-muted transition-colors hover:text-fg disabled:opacity-50"
        :disabled="busy"
        title="Kill this shell and start a fresh one"
        @click="restart"
      >
        {{ busy ? 'Restarting…' : 'Restart' }}
      </button>
    </header>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>

    <div class="min-h-0 flex-1">
      <AgentTerminal :key="epoch" ws-path="/api/shell/terminal" class="h-full" />
    </div>
  </div>
</template>
