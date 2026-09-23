<script setup lang="ts">
import { onMounted, ref } from 'vue';
import * as api from '../api';
import type { CreatedToken, Token } from '../types';
import { confirmAction } from '../lib/confirmation';

const tokens = ref<Token[]>([]);
const created = ref<CreatedToken | null>(null);
const name = ref('production');
const error = ref('');
const busy = ref(false);

async function load() {
  try {
    tokens.value = await api.listDeploymentTokens();
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  }
}

async function create() {
  if (!name.value.trim() || busy.value) return;
  busy.value = true;
  created.value = null;
  try {
    created.value = await api.createDeploymentToken(name.value.trim());
    await load();
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = false;
  }
}

async function revoke(token: Token) {
  await confirmAction({
    title: `Revoke deployment token "${token.name}"?`,
    description: 'The next deployment using this token will fail.',
    confirmLabel: 'Revoke token',
    danger: true,
    action: async () => {
      busy.value = true;
      try {
        await api.revokeDeploymentToken(token.id);
        await load();
      } finally {
        busy.value = false;
      }
    },
  });
}

onMounted(load);
</script>

<template>
  <section>
    <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
      Deployment tokens
    </h2>
    <p class="mb-3 text-xs text-faint">
      These tokens can only apply the deployment manifest. Store the secret in Secret Manager for
      your deploy host; Loom shows it only once.
    </p>
    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>
    <div
      v-if="created"
      data-testid="deployment-token-secret"
      class="mb-3 rounded-md border border-accent bg-surface p-2.5"
    >
      <p class="mb-2 text-xs font-medium text-accent">
        Copy this token now. It will not be shown again.
      </p>
      <code class="block select-all break-all rounded bg-input px-2 py-1 font-mono text-xs">{{
        created.token
      }}</code>
    </div>
    <div class="mb-3 flex gap-2 rounded-md border border-line bg-surface px-3 py-2.5">
      <input
        v-model="name"
        data-testid="deployment-token-name"
        aria-label="Deployment token name"
        class="min-w-0 flex-1 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        @keyup.enter="create"
      />
      <button
        data-testid="deployment-token-create"
        class="btn-primary px-2.5 py-1 text-xs"
        :disabled="busy || !name.trim()"
        @click="create"
      >
        Create token
      </button>
    </div>
    <div
      v-for="token in tokens"
      :key="token.id"
      data-testid="deployment-token-row"
      class="flex items-center gap-2 border-b border-line py-2 text-xs"
    >
      <span class="font-medium">{{ token.name }}</span>
      <code class="text-faint">{{ token.prefix }}…</code>
      <span class="text-faint"
        >Last used
        {{ token.last_used_at ? new Date(token.last_used_at).toLocaleString() : 'never' }}</span
      >
      <button
        data-testid="deployment-token-revoke"
        class="btn-secondary ml-auto px-2 py-1 text-xs"
        :disabled="busy"
        @click="revoke(token)"
      >
        Revoke
      </button>
    </div>
    <p v-if="!tokens.length" class="text-xs text-muted">No deployment tokens yet.</p>
  </section>
</template>
