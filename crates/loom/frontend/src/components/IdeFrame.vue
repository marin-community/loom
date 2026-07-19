<script setup lang="ts">
// The embedded VS Code (code-server) for one session, in an <iframe>. The
// editor is served by loom's reverse proxy on the page origin — under loom's
// auth, so the iframe rides the same session cookie and code-server needs no
// password of its own. `?folder=` opens the worktree straight in the Explorer.
//
// This is the file/editing surface (it replaced the bespoke Files browser). If
// code-server isn't installed on the loom host, ide-info reports it and we show
// a short hint instead of a broken frame.
import { ref, computed, onMounted } from 'vue';
import { ideInfo } from '../api';
import type { IdeInfo } from '../types';

const props = defineProps<{ id: string; workDir: string }>();

type State = 'loading' | 'ready' | 'disabled' | 'unavailable' | 'error';
const state = ref<State>('loading');
const errorMsg = ref('');
// Bumping this key remounts the iframe — a manual reload, or a fresh spawn
// after the server idle-reaped the previous code-server.
const reloadKey = ref(0);

// Trailing slash is load-bearing: code-server derives its base path from the
// request's relative URL, so it must be mounted at `…/ide/`, not `…/ide`.
const src = computed(
  () => `/api/sessions/${props.id}/ide/?folder=${encodeURIComponent(props.workDir)}`,
);

async function probe() {
  state.value = 'loading';
  try {
    const info: IdeInfo = await ideInfo(props.id);
    state.value = !info.enabled ? 'disabled' : !info.available ? 'unavailable' : 'ready';
  } catch (e) {
    state.value = 'error';
    errorMsg.value = (e as Error).message;
  }
}

function reload() {
  reloadKey.value += 1;
  probe();
}

defineExpose({ reload });
onMounted(probe);
</script>

<template>
  <div class="flex h-full min-h-0 flex-col bg-code">
    <div class="flex items-center gap-2 border-b border-line px-2 py-1 text-xs">
      <span class="font-medium text-fg">Editor</span>
      <span class="min-w-0 truncate font-mono text-faint" :title="workDir">{{ workDir }}</span>
      <button
        class="ml-auto shrink-0 rounded px-1.5 py-0.5 text-muted hover:bg-subtle hover:text-fg"
        title="Reload editor"
        aria-label="Reload editor"
        @click="reload"
      >
        ⟳
      </button>
    </div>

    <div class="relative min-h-0 flex-1">
      <iframe
        v-if="state === 'ready'"
        :key="reloadKey"
        :src="src"
        class="h-full w-full border-0"
        title="VS Code"
        allow="clipboard-read; clipboard-write"
      ></iframe>

      <div
        v-else
        class="flex h-full w-full flex-col items-center justify-center gap-2 p-6 text-center text-sm"
      >
        <p v-if="state === 'loading'" class="text-muted">Starting editor…</p>

        <template v-else-if="state === 'unavailable'">
          <p class="font-medium text-fg">code-server isn't installed</p>
          <p class="max-w-sm text-muted">
            The embedded editor needs <code class="font-mono text-fg">code-server</code> on the loom
            host. Install it and reopen this panel.
          </p>
          <code class="rounded bg-subtle px-2 py-1 font-mono text-xs text-muted"
            >curl -fsSL https://code-server.dev/install.sh | sh</code
          >
        </template>

        <p v-else-if="state === 'disabled'" class="text-muted">
          The embedded editor is disabled in settings.
        </p>

        <template v-else>
          <p class="text-block">{{ errorMsg || 'Could not load the editor.' }}</p>
          <button class="rounded bg-subtle px-2 py-1 text-muted hover:text-fg" @click="reload">
            Retry
          </button>
        </template>
      </div>
    </div>
  </div>
</template>
