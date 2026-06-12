<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { useRouter } from 'vue-router';
import * as api from '../api';
import { me, doLogout } from '../auth';
import type { User, GithubConfig } from '../types';

// Account + access management: who you are, your password, the approved-user
// allowlist, and the GitHub OAuth app config that powers "Continue with GitHub".
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

// -- Approved users ---------------------------------------------------------
const users = ref<User[]>([]);
const newUser = ref('');
const newUserGithub = ref('');
const newUserPassword = ref('');

async function loadUsers() {
  try {
    users.value = await api.listUsers();
  } catch (e) {
    fail(e);
  }
}

async function addUser() {
  if (!newUser.value.trim()) return;
  busy.value = true;
  try {
    await api.addUser(
      newUser.value.trim(),
      newUserGithub.value.trim() || undefined,
      newUserPassword.value || undefined,
    );
    newUser.value = '';
    newUserGithub.value = '';
    newUserPassword.value = '';
    ok('User approved.');
    await loadUsers();
  } catch (e) {
    fail(e);
  } finally {
    busy.value = false;
  }
}

async function removeUser(u: User) {
  if (!confirm(`Remove approved user "${u.username}"? They will lose access.`)) return;
  busy.value = true;
  try {
    await api.removeUser(u.username);
    ok('User removed.');
    await loadUsers();
  } catch (e) {
    fail(e);
  } finally {
    busy.value = false;
  }
}

// -- GitHub OAuth app -------------------------------------------------------
const gh = ref<GithubConfig | null>(null);
const ghClientId = ref('');
const ghClientSecret = ref('');

async function loadGithub() {
  try {
    gh.value = await api.getGithubConfig();
    ghClientId.value = gh.value.client_id;
  } catch (e) {
    fail(e);
  }
}

async function saveGithub() {
  busy.value = true;
  try {
    // Send the secret only when the field was filled, so an empty field leaves
    // the stored secret intact.
    gh.value = await api.setGithubConfig(
      ghClientId.value.trim(),
      ghClientSecret.value ? ghClientSecret.value : undefined,
    );
    ghClientSecret.value = '';
    ok('GitHub sign-in updated.');
  } catch (e) {
    fail(e);
  } finally {
    busy.value = false;
  }
}

async function logout() {
  await doLogout();
  router.push('/login');
}

onMounted(() => {
  loadUsers();
  loadGithub();
});
</script>

<template>
  <div class="space-y-6">
    <p v-if="error" class="text-sm text-block">{{ error }}</p>
    <p v-if="notice" class="text-sm text-accent">{{ notice }}</p>

    <!-- Identity -->
    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">Signed in</h2>
      <div class="flex items-center justify-between rounded-md border border-line bg-surface px-3 py-2.5">
        <div>
          <p class="text-sm font-medium">{{ me.username }}</p>
          <p class="text-2xs text-faint">
            <template v-if="me.github_login">GitHub: {{ me.github_login }} · </template>
            via {{ me.via }}
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

    <!-- GitHub sign-in -->
    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        GitHub sign-in
      </h2>
      <div class="rounded-md border border-line bg-surface px-3 py-2.5">
        <p class="text-xs text-muted mb-2">
          Register an OAuth app on GitHub with the callback
          <code class="font-mono">{{ gh?.callback_path }}</code>, then paste its id and secret.
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
            @click="saveGithub"
          >
            Save
          </button>
        </div>
      </div>
    </section>

    <!-- Approved users -->
    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        Approved users
      </h2>
      <div class="overflow-hidden rounded-md border border-line bg-surface">
        <div
          v-for="u in users"
          :key="u.username"
          class="flex items-center gap-3 border-b border-line px-3 py-2.5 last:border-0"
        >
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">{{ u.username }}</p>
            <p class="text-2xs text-faint">
              <template v-if="u.github_login">GitHub: {{ u.github_login }}</template>
              <template v-else>no GitHub login</template>
              · {{ u.has_password ? 'password set' : 'no password' }}
            </p>
          </div>
          <button
            v-if="u.username !== me.username"
            class="btn-secondary px-2.5 py-1 text-xs"
            :disabled="busy"
            @click="removeUser(u)"
          >
            Remove
          </button>
          <span v-else class="text-2xs text-faint">you</span>
        </div>
      </div>

      <div class="mt-2 flex flex-wrap items-end gap-2">
        <input
          v-model="newUser"
          placeholder="Username"
          class="rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        />
        <input
          v-model="newUserGithub"
          placeholder="GitHub login (optional)"
          class="rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        />
        <input
          v-model="newUserPassword"
          type="password"
          autocomplete="new-password"
          placeholder="Password (optional)"
          class="rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
        />
        <button
          class="btn-primary px-3 py-1.5 text-xs"
          :disabled="busy || !newUser.trim()"
          @click="addUser"
        >
          Approve user
        </button>
      </div>
    </section>
  </div>
</template>
