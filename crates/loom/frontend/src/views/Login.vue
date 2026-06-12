<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import * as api from '../api';
import { me, loadMe } from '../auth';

// The chrome-free sign-in screen (App.vue renders this bare, without the nav
// rail, whenever the caller is unauthenticated). Offers GitHub sign-in when an
// OAuth app is configured, plus username/password.
const route = useRoute();
const router = useRouter();

const username = ref('');
const password = ref('');
const busy = ref(false);
const error = ref('');

// Friendly text for the codes the GitHub callback redirects back with.
const OAUTH_ERRORS: Record<string, string> = {
  'not-approved': 'That GitHub account is not on the approved list.',
  'state-mismatch': 'The sign-in attempt expired or could not be verified. Try again.',
  'missing-code': 'GitHub did not return an authorization code. Try again.',
};

onMounted(() => {
  const code = route.query.error as string | undefined;
  if (code) error.value = OAUTH_ERRORS[code] ?? `Sign-in failed (${code}).`;
  if (me.authenticated) router.replace('/');
});

async function submit() {
  if (busy.value) return;
  busy.value = true;
  error.value = '';
  try {
    await api.login(username.value, password.value);
    await loadMe();
    router.replace((route.query.redirect as string) || '/');
  } catch (e) {
    error.value = (e as Error).message || 'Sign-in failed';
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-canvas px-4 font-sans text-fg">
    <div class="w-full max-w-sm">
      <div class="mb-6 flex items-center justify-center gap-2">
        <span class="text-accent">
          <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor"
            stroke-width="1.75" stroke-linecap="round" aria-hidden="true">
            <path d="M4 9h16M4 15h16M9 4v16M15 4v16" />
          </svg>
        </span>
        <span class="text-lg font-semibold tracking-tight">loom</span>
      </div>

      <div class="rounded-lg border border-line bg-surface p-5">
        <h1 class="mb-1 text-sm font-semibold">Sign in</h1>
        <p class="mb-4 text-xs text-muted">Authenticate to manage the fleet.</p>

        <p v-if="error" class="mb-3 rounded bg-input px-2.5 py-1.5 text-xs text-block">{{ error }}</p>

        <a
          v-if="me.methods.github"
          :href="api.githubLoginUrl"
          class="btn-secondary mb-3 flex w-full items-center justify-center gap-2 py-2 text-sm"
        >
          <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
            <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
          </svg>
          Continue with GitHub
        </a>

        <div v-if="me.methods.github" class="my-3 flex items-center gap-2 text-2xs text-faint">
          <span class="h-px flex-1 bg-line"></span>or<span class="h-px flex-1 bg-line"></span>
        </div>

        <form class="space-y-2.5" @submit.prevent="submit">
          <input
            v-model="username"
            autocomplete="username"
            placeholder="Username"
            class="w-full rounded bg-input px-2.5 py-2 text-sm outline-none focus:ring-1 ring-accent"
          />
          <input
            v-model="password"
            type="password"
            autocomplete="current-password"
            placeholder="Password"
            class="w-full rounded bg-input px-2.5 py-2 text-sm outline-none focus:ring-1 ring-accent"
          />
          <button
            type="submit"
            :disabled="busy || !username || !password"
            class="btn-primary w-full py-2 text-sm"
          >
            {{ busy ? 'Signing in…' : 'Sign in' }}
          </button>
        </form>
      </div>
    </div>
  </div>
</template>
