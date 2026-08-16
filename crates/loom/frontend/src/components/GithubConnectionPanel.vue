<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import * as api from '../api';
import type { GithubConfig } from '../types';

const gh = ref<GithubConfig | null>(null);
const ghClientId = ref('');
const ghClientSecret = ref('');
const error = ref('');
const notice = ref('');
const busy = ref(false);

const appUrl = computed(() =>
  gh.value?.app_slug ? `https://github.com/apps/${gh.value.app_slug}` : '',
);

async function load() {
  try {
    gh.value = await api.getGithubConfig();
    ghClientId.value = gh.value.client_id;
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  }
}

async function save() {
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    gh.value = await api.setGithubConfig(
      ghClientId.value.trim(),
      ghClientSecret.value || undefined,
    );
    ghClientSecret.value = '';
    notice.value = 'GitHub connection updated.';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = false;
  }
}

onMounted(load);
</script>

<template>
  <section class="space-y-2" data-testid="github-connection-panel">
    <div>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        Loom GitHub App
      </h2>
      <p class="text-xs text-faint">
        Deployment-owned integration for GitHub sign-in, <code class="font-mono">@loom</code>
        triggers, and short-lived repository credentials. Personal GitHub tokens remain under
        <span class="font-medium">Account</span>.
      </p>
    </div>
    <p v-if="error" class="text-sm text-block">{{ error }}</p>
    <p v-if="notice" class="text-sm text-accent">{{ notice }}</p>
    <div class="rounded-md border border-line bg-surface px-3 py-2.5">
      <div v-if="gh?.app_configured" class="mb-2">
        <p class="text-sm">
          <span class="text-accent">✓</span>
          <a
            v-if="appUrl"
            :href="appUrl"
            target="_blank"
            rel="noopener"
            class="font-medium text-accent hover:underline"
            >{{ gh.app_slug }}</a
          >
          <span v-else class="font-medium">GitHub App</span>
          <span class="text-faint"> · App ID {{ gh.app_id }}</span>
        </p>
        <p class="text-xs text-muted mt-0.5">
          Manage the App, its installations, and its private credentials with
          <code class="font-mono">loom setup github-app</code> or deployment IaC.
        </p>
      </div>
      <p v-else class="text-xs text-muted mb-2">
        No GitHub App configured. Run
        <code class="font-mono">loom setup github-app --base-url &lt;your loom URL&gt;</code>
        to register one for sign-in and the <code class="font-mono">@loom</code> trigger. You can
        also paste sign-in credentials below.
      </p>

      <p class="text-2xs font-semibold uppercase tracking-wider text-muted mt-3 mb-1">
        Sign-in credentials
      </p>
      <p class="text-xs text-muted mb-2">
        <template v-if="gh?.app_configured">The same App's</template>
        <template v-else>The</template>
        OAuth client, with callback <code class="font-mono">{{ gh?.callback_path }}</code
        >. Powers "Continue with GitHub".
        <span :class="gh?.configured ? 'text-accent' : 'text-faint'">
          {{ gh?.configured ? 'Configured.' : 'Not configured.' }}
        </span>
      </p>
      <div class="space-y-2">
        <input
          v-model="ghClientId"
          placeholder="Client ID"
          class="w-full rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        />
        <input
          v-model="ghClientSecret"
          type="password"
          :placeholder="gh?.configured ? 'Client secret (leave blank to keep)' : 'Client secret'"
          class="w-full rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        />
        <button
          class="btn-primary px-3 py-1.5 text-xs"
          :disabled="busy || !ghClientId.trim()"
          @click="save"
        >
          Save
        </button>
      </div>
    </div>
  </section>
</template>
