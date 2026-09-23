<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { useRouter } from 'vue-router';
import * as api from '../api';
import { me, doLogout } from '../auth';
import { confirmAction } from '../lib/confirmation';

// Personal account and access management. Deployment connections such as the
// Loom GitHub App lives in Integrations instead.
const router = useRouter();
const error = ref('');
const notice = ref('');
const busy = ref(false);

function ok(message: string) {
  notice.value = message;
  error.value = '';
}
function fail(e: unknown) {
  error.value = (e as Error).message;
  notice.value = '';
}

// -- Password ---------------------------------------------------------------
const newPassword = ref('');
const confirmPassword = ref('');

async function savePassword() {
  if (newPassword.value.length < 8) {
    fail(new Error('Password must be at least 8 characters.'));
    return;
  }
  if (newPassword.value !== confirmPassword.value) {
    fail(new Error('Passwords do not match.'));
    return;
  }
  busy.value = true;
  try {
    await api.setPassword(newPassword.value);
    newPassword.value = '';
    confirmPassword.value = '';
    ok('Password updated.');
  } catch (e) {
    fail(e);
  } finally {
    busy.value = false;
  }
}

// -- Your GitHub token ------------------------------------------------------
// This Loom-owned Account credential is selected for ordinary interactive
// sessions launched by this user.
const PAT_CREATE_URL =
  'https://github.com/settings/personal-access-tokens/new' +
  '?name=Loom' +
  '&description=Interactive%20Loom%20sessions' +
  '&contents=write&issues=write&pull_requests=write';
const ghToken = ref('');
const ghTokenStatus = ref<api.GithubTokenStatus | null>(null);

async function loadMyGithubToken() {
  try {
    ghTokenStatus.value = await api.getMyGithubToken();
  } catch (e) {
    fail(e);
  }
}

async function saveMyGithubToken() {
  if (!ghToken.value.trim()) return;
  busy.value = true;
  try {
    ghTokenStatus.value = await api.setMyGithubToken(ghToken.value.trim());
    ghToken.value = '';
    ok('GitHub token saved — your new interactive sessions will act as you.');
  } catch (e) {
    fail(e);
  } finally {
    busy.value = false;
  }
}

function daysAgo(iso: string): string {
  const days = Math.floor((Date.now() - new Date(iso).getTime()) / (24 * 60 * 60 * 1000));
  if (days <= 0) return 'today';
  return `${days} day${days === 1 ? '' : 's'} ago`;
}

async function clearMyGithubToken() {
  await confirmAction({
    title: 'Remove your personal GitHub token?',
    description:
      "New interactive sessions will use the selected profile's GitHub App access. Existing sessions are unchanged.",
    confirmLabel: 'Remove token',
    danger: true,
    action: async () => {
      busy.value = true;
      try {
        await api.deleteMyGithubToken();
        ghTokenStatus.value = { set: false, updated_at: null, last8: null };
        ok('GitHub token removed.');
      } finally {
        busy.value = false;
      }
    },
  });
}

async function logout() {
  await doLogout();
  router.push('/login');
}

onMounted(loadMyGithubToken);
</script>

<template>
  <div class="space-y-6">
    <p v-if="error" class="text-sm text-block">{{ error }}</p>
    <p v-if="notice" class="text-sm text-accent">{{ notice }}</p>

    <!-- Identity -->
    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">Signed in</h2>
      <div
        class="flex items-center justify-between rounded-md border border-line bg-surface px-3 py-2.5"
      >
        <div>
          <p class="text-sm font-medium">{{ me.username }}</p>
          <p class="text-2xs text-faint">
            <template v-if="me.github_login">GitHub: {{ me.github_login }} · </template>
            {{ me.role === 'admin' ? 'Admin' : 'User' }} · via {{ me.via }}
          </p>
        </div>
        <button class="btn-secondary px-2.5 py-1 text-xs" @click="logout">Sign out</button>
      </div>
    </section>

    <!-- Password -->
    <section v-if="me.authorization_kind === 'manual'">
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">Password</h2>
      <div class="rounded-md border border-line bg-surface px-3 py-2.5">
        <p class="text-xs text-muted mb-2">
          Set a password to sign in without GitHub. At least 8 characters.
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <input
            v-model="newPassword"
            type="password"
            autocomplete="new-password"
            placeholder="New password"
            class="flex-1 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
          />
          <input
            v-model="confirmPassword"
            type="password"
            autocomplete="new-password"
            placeholder="Confirm"
            class="flex-1 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
          />
          <button
            class="btn-primary px-3 py-1.5 text-xs"
            :disabled="busy || !newPassword"
            @click="savePassword"
          >
            Update
          </button>
        </div>
      </div>
    </section>

    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        Your GitHub token
      </h2>
      <div class="rounded-md border border-line bg-surface px-3 py-2.5">
        <p class="text-xs text-muted mb-2">
          An optional personal fine-grained token Loom stores for you. Loom injects it into your
          ordinary interactive sessions so <code class="font-mono">git push</code> and
          <code class="font-mono">gh</code> act as you. When it is not set, new sessions use the
          selected profile’s approved GitHub App access.
          <a class="text-accent underline" :href="PAT_CREATE_URL" target="_blank" rel="noopener">
            Create one</a
          >
          with <span class="font-medium">Contents</span>, <span class="font-medium">Issues</span>,
          and <span class="font-medium">Pull requests</span> read/write. Repository selection and
          permissions are separate; choose the repositories your sessions use. Add
          <span class="font-medium">Workflows</span> read/write only when sessions must edit
          <code class="font-mono">.github/workflows</code>.
          <span v-if="!ghTokenStatus?.set" class="text-faint">
            Not set — using GitHub App access.
          </span>
        </p>
        <div
          v-if="ghTokenStatus?.set"
          class="mb-2 flex items-center gap-2 rounded bg-input px-2 py-1"
          data-testid="github-token-current"
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            class="shrink-0 text-faint"
          >
            <path
              d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"
            ></path>
          </svg>
          <span class="font-mono text-xs text-accent">*****{{ ghTokenStatus.last8 }}</span>
          <span v-if="ghTokenStatus.updated_at" class="text-2xs text-faint">
            (Added {{ daysAgo(ghTokenStatus.updated_at) }})
          </span>
          <button
            type="button"
            class="ml-auto text-faint hover:text-block disabled:opacity-50"
            :disabled="busy"
            title="Remove GitHub token"
            aria-label="Remove GitHub token"
            data-testid="github-token-delete"
            @click="clearMyGithubToken"
          >
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <polyline points="3 6 5 6 21 6"></polyline>
              <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"></path>
              <path d="M10 11v6"></path>
              <path d="M14 11v6"></path>
              <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"></path>
            </svg>
          </button>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          <input
            v-model="ghToken"
            type="password"
            autocomplete="off"
            placeholder="github_pat_…"
            class="flex-1 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
            @keyup.enter="saveMyGithubToken"
          />
          <button
            class="btn-primary px-3 py-1.5 text-xs"
            :disabled="busy || !ghToken.trim()"
            @click="saveMyGithubToken"
          >
            {{ ghTokenStatus?.set ? 'Replace' : 'Save' }}
          </button>
        </div>
      </div>
    </section>
  </div>
</template>
