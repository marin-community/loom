<script setup lang="ts">
import { ref, onMounted } from 'vue';
import * as api from '../api';
import type { Token, CreatedToken } from '../types';

// Manage API tokens — the `LOOM_TOKEN` automation presents as a bearer. The
// plaintext is shown once at creation; thereafter only metadata is listed.
const tokens = ref<Token[]>([]);
const error = ref('');
const busy = ref(false);
const name = ref('');
const expiresDays = ref('');
const created = ref<CreatedToken | null>(null);
const copied = ref(false);

async function load() {
  try {
    tokens.value = await api.listTokens();
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function create() {
  if (!name.value.trim() || busy.value) return;
  busy.value = true;
  error.value = '';
  created.value = null;
  try {
    const days = expiresDays.value.trim() ? Number(expiresDays.value) : null;
    created.value = await api.createToken(name.value.trim(), days);
    name.value = '';
    expiresDays.value = '';
    copied.value = false;
    await load();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function revoke(t: Token) {
  if (!confirm(`Revoke "${t.name}"? Anything using this token will stop working.`)) return;
  busy.value = true;
  error.value = '';
  try {
    await api.revokeToken(t.id);
    await load();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function copy() {
  if (!created.value) return;
  try {
    await navigator.clipboard.writeText(created.value.token);
    copied.value = true;
  } catch {
    /* clipboard may be unavailable; the secret is still selectable */
  }
}

const fmt = (ts: string | null) => (ts ? new Date(ts).toLocaleString() : '—');

onMounted(load);
</script>

<template>
  <div>
    <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">API tokens</h2>
    <p class="text-xs text-faint mb-3">
      A token authenticates automation and remote CLIs. Send it as
      <code>Authorization: Bearer</code> or set it as <code>LOOM_TOKEN</code> (with
      <code>WEAVER_API</code> pointing at this server).
    </p>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>

    <!-- One-time secret: shown once, right after creation. -->
    <div v-if="created" class="mb-4 rounded-md border border-accent bg-surface p-3" data-testid="new-token">
      <p class="text-xs font-medium text-accent mb-1">
        Copy this token now — it won't be shown again.
      </p>
      <div class="flex items-center gap-2">
        <code class="flex-1 select-all break-all rounded bg-input px-2 py-1.5 font-mono text-xs">{{
          created.token
        }}</code>
        <button class="btn-secondary px-2.5 py-1 text-xs" @click="copy">
          {{ copied ? 'Copied' : 'Copy' }}
        </button>
      </div>
    </div>

    <!-- Create form. -->
    <div class="mb-4 flex flex-wrap items-end gap-2">
      <label class="flex flex-col gap-1">
        <span class="text-2xs text-muted">Name</span>
        <input
          v-model="name"
          placeholder="e.g. github-actions"
          data-testid="token-name"
          class="rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
          @keyup.enter="create"
        />
      </label>
      <label class="flex flex-col gap-1">
        <span class="text-2xs text-muted">Expires (days)</span>
        <input
          v-model="expiresDays"
          type="number"
          min="1"
          placeholder="never"
          class="w-28 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        />
      </label>
      <button
        class="btn-primary px-3 py-1.5 text-xs"
        :disabled="busy || !name.trim()"
        data-testid="token-create"
        @click="create"
      >
        Create token
      </button>
    </div>

    <!-- Existing tokens. -->
    <div v-if="tokens.length" class="overflow-hidden rounded-md border border-line bg-surface">
      <div
        v-for="t in tokens"
        :key="t.id"
        class="flex items-center gap-3 border-b border-line px-3 py-2.5 last:border-0"
        data-testid="token-row"
      >
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <span class="truncate text-sm font-medium">{{ t.name }}</span>
            <code class="font-mono text-2xs text-faint">{{ t.prefix }}…</code>
          </div>
          <p class="text-2xs text-faint">
            Created {{ fmt(t.created_at) }} · Last used {{ fmt(t.last_used_at) }}
            <template v-if="t.expires_at"> · Expires {{ fmt(t.expires_at) }}</template>
          </p>
        </div>
        <button
          class="btn-secondary px-2.5 py-1 text-xs"
          :disabled="busy"
          data-testid="token-revoke"
          @click="revoke(t)"
        >
          Revoke
        </button>
      </div>
    </div>
    <p v-else class="text-sm text-muted">No tokens yet.</p>
  </div>
</template>
