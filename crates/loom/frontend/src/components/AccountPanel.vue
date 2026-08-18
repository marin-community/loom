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
// This is Loom-owned user state, not an ambient process credential. Loom
// injects it only into ordinary interactive sessions launched by this user.
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
        ghTokenStatus.value = { set: false, updated_at: null };
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
    <section>
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
          <span :class="ghTokenStatus?.set ? 'text-accent' : 'text-faint'">
            {{ ghTokenStatus?.set ? 'Set.' : 'Not set — using GitHub App access.' }}
          </span>
        </p>
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
            Save
          </button>
          <button
            v-if="ghTokenStatus?.set"
            class="btn-secondary px-2.5 py-1 text-xs"
            :disabled="busy"
            @click="clearMyGithubToken"
          >
            Clear
          </button>
        </div>
      </div>
    </section>
  </div>
</template>
