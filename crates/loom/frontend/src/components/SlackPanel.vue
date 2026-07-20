<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import * as api from '../api';
import type { SlackStatus } from '../types';

// Read-only Slack connection state for the Connections settings pane — the
// bot-token `auth.test` result, alongside GitHub's identity block above. The
// tokens themselves are never exposed here (set via `LOOM_SLACK_APP_TOKEN` /
// `LOOM_SLACK_BOT_TOKEN` in the server environment, outside the settings
// registry); this card only reports whether they work. `slack.enabled` is the
// kill switch, editable as a regular setting below.
const status = ref<SlackStatus | null>(null);
const error = ref('');

async function load() {
  try {
    status.value = await api.getSlackStatus();
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

// Dot + label: sage once `auth.test` succeeds, ochre once configured but
// failing (the same "wants a look" tone the fleet reserves for attention),
// faint when the tokens aren't set at all.
const indicator = computed(() => {
  if (!status.value) return { dot: 'bg-faint/50', text: 'text-faint', label: 'Checking…' };
  if (status.value.connected) return { dot: 'bg-ok-line', text: 'text-ok', label: 'Connected' };
  if (status.value.configured) {
    return { dot: 'bg-attn-line', text: 'text-attn', label: 'Not connected' };
  }
  return { dot: 'bg-faint/50', text: 'text-faint', label: 'Not configured' };
});
</script>

<template>
  <div>
    <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">Slack</h2>
    <div class="rounded-md border border-line bg-surface px-3 py-2.5">
      <p v-if="error" class="text-sm text-block">{{ error }}</p>

      <template v-else>
        <div class="flex items-center gap-1.5">
          <span class="h-1.5 w-1.5 rounded-full" :class="indicator.dot" aria-hidden="true"></span>
          <span class="text-sm font-medium" :class="indicator.text">{{ indicator.label }}</span>
          <span v-if="status?.configured && !status.enabled" class="text-2xs text-faint">
            · disabled
          </span>
        </div>

        <p v-if="status?.connected" class="mt-1 text-xs text-muted">
          Bot <code class="font-mono">{{ status.bot_user }}</code> in workspace
          <code class="font-mono">{{ status.team }}</code
          >.
        </p>
        <p v-else-if="status?.configured && status.error" class="mt-1 text-xs text-block">
          {{ status.error }}
        </p>
        <p v-else-if="status && !status.configured" class="mt-1 text-xs text-muted">
          Set <code class="font-mono">LOOM_SLACK_APP_TOKEN</code> and
          <code class="font-mono">LOOM_SLACK_BOT_TOKEN</code> in the server environment to enable
          the <code class="font-mono">@loom</code> Slack trigger.
        </p>
      </template>
    </div>
  </div>
</template>
