<script setup lang="ts">
import { onMounted, ref } from 'vue';
import * as api from '../api';
import { loadMe, me } from '../auth';
import type { User, UserRole } from '../types';
import { confirmAction } from '../lib/confirmation';

const users = ref<User[]>([]);
const newUser = ref('');
const newUserGithub = ref('');
const newUserGithubId = ref('');
const newUserPassword = ref('');
const newUserRole = ref<UserRole>('user');
const busy = ref('');
const error = ref('');
const notice = ref('');
const githubIdentityIds = ref<Record<string, string>>({});

async function load() {
  try {
    users.value = await api.listUsers();
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  }
}

async function add() {
  const username = newUser.value.trim();
  if (!username || busy.value) return;
  busy.value = 'add';
  error.value = '';
  notice.value = '';
  try {
    await api.addUser(
      username,
      newUserGithub.value.trim() || undefined,
      newUserGithubId.value ? Number(newUserGithubId.value) : undefined,
      newUserPassword.value || undefined,
      newUserRole.value,
    );
    newUser.value = '';
    newUserGithub.value = '';
    newUserGithubId.value = '';
    newUserPassword.value = '';
    newUserRole.value = 'user';
    notice.value = `${username} approved.`;
    await load();
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = '';
  }
}

async function changeRole(user: User, role: UserRole) {
  if (role === user.role || busy.value) return;
  busy.value = `role:${user.username}`;
  error.value = '';
  notice.value = '';
  try {
    const updated = await api.setUserRole(user.username, role);
    users.value = users.value.map((entry) =>
      entry.username === updated.username ? updated : entry,
    );
    if (user.username === me.username) await loadMe();
    notice.value = `${user.username} is now ${role}.`;
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = '';
  }
}

async function approveManually(user: User) {
  if (busy.value) return;
  busy.value = `approve:${user.username}`;
  error.value = '';
  notice.value = '';
  try {
    const updated = await api.approveUserManually(user.username);
    users.value = users.value.map((entry) =>
      entry.username === updated.username ? updated : entry,
    );
    notice.value = `${user.username} is now approved manually.`;
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = '';
  }
}

async function bindGithubIdentity(user: User) {
  const id = Number(githubIdentityIds.value[user.username]);
  if (!user.github_login || !Number.isInteger(id) || id <= 0 || busy.value) return;
  busy.value = `identity:${user.username}`;
  error.value = '';
  notice.value = '';
  try {
    const updated = await api.setUserGithubIdentity(user.username, user.github_login, id);
    users.value = users.value.map((entry) =>
      entry.username === updated.username ? updated : entry,
    );
    delete githubIdentityIds.value[user.username];
    notice.value = `${user.username}'s GitHub identity is bound.`;
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    busy.value = '';
  }
}

async function remove(user: User) {
  await confirmAction({
    title: `Remove approved user "${user.username}"?`,
    description:
      user.authorization_kind === 'github_organization'
        ? 'Their Loom credentials will stop working, but active organization membership lets them sign in again.'
        : 'They will lose dashboard and API access immediately.',
    confirmLabel: 'Remove user',
    danger: true,
    action: async () => {
      busy.value = `remove:${user.username}`;
      error.value = '';
      notice.value = '';
      try {
        await api.removeUser(user.username);
        users.value = users.value.filter((entry) => entry.username !== user.username);
        notice.value = `${user.username} removed.`;
      } finally {
        busy.value = '';
      }
    },
  });
}

onMounted(load);
</script>

<template>
  <section class="space-y-3">
    <div>
      <h2 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-muted">
        Approved users
      </h2>
      <p class="text-xs text-faint">
        Users can operate sessions, repositories, and reviews, and inspect watches and diagnostics.
        Admins can also change deployment-wide policy, integrations, watches, and access.
      </p>
    </div>
    <p v-if="error" class="text-sm text-block" role="alert">{{ error }}</p>
    <p v-if="notice" class="text-sm text-accent">{{ notice }}</p>

    <div class="overflow-hidden rounded-md border border-line bg-surface">
      <div
        v-for="user in users"
        :key="user.username"
        class="flex flex-wrap items-center gap-3 border-b border-line px-3 py-2.5 last:border-0"
      >
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-medium">
            {{ user.username }}
            <span v-if="user.username === me.username" class="text-2xs font-normal text-faint"
              >you</span
            >
          </p>
          <p class="text-2xs text-faint">
            {{ user.github_login ? `GitHub: ${user.github_login}` : 'no GitHub login' }} ·
            {{ user.has_password ? 'password set' : 'no password' }}
          </p>
          <p v-if="user.authorization_kind === 'github_organization'" class="text-2xs text-faint">
            Organization: {{ user.authorization_github_org_login }} · access through
            {{ new Date(user.authorization_valid_until!).toLocaleString() }}
          </p>
          <p v-else class="text-2xs text-faint">Manually approved</p>
          <div
            v-if="user.authorization_kind === 'manual' && user.github_login && !user.github_user_id"
            class="mt-2 flex max-w-md items-end gap-2"
          >
            <label class="flex-1 text-2xs text-muted">
              Numeric GitHub user ID required before GitHub sign-in
              <input
                v-model="githubIdentityIds[user.username]"
                type="number"
                min="1"
                class="mt-1 w-full rounded bg-input px-2 py-1 text-xs"
              />
            </label>
            <button
              class="btn-secondary px-2.5 py-1 text-xs"
              :disabled="Boolean(busy) || !githubIdentityIds[user.username]"
              @click="bindGithubIdentity(user)"
            >
              Bind identity
            </button>
          </div>
        </div>
        <select
          :value="user.role"
          :disabled="Boolean(busy) || user.authorization_kind === 'github_organization'"
          :aria-label="`Role for ${user.username}`"
          class="rounded bg-input px-2 py-1 text-xs"
          @change="changeRole(user, ($event.target as HTMLSelectElement).value as UserRole)"
        >
          <option value="user">User</option>
          <option value="admin">Admin</option>
        </select>
        <button
          v-if="user.authorization_kind === 'github_organization'"
          class="btn-secondary px-2.5 py-1 text-xs"
          :disabled="Boolean(busy)"
          @click="approveManually(user)"
        >
          Approve manually
        </button>
        <button
          v-if="user.username !== me.username"
          class="btn-secondary px-2.5 py-1 text-xs"
          :disabled="Boolean(busy)"
          @click="remove(user)"
        >
          Remove
        </button>
      </div>
    </div>

    <div class="grid gap-2 rounded-md border border-line bg-surface p-3 md:grid-cols-2">
      <label class="text-2xs text-muted">
        Username
        <input
          v-model="newUser"
          placeholder="Username"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 text-sm"
        />
      </label>
      <label class="text-2xs text-muted">
        GitHub login
        <input
          v-model="newUserGithub"
          placeholder="Optional"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 text-sm"
        />
      </label>
      <label class="text-2xs text-muted">
        GitHub numeric user ID
        <input
          v-model="newUserGithubId"
          type="number"
          min="1"
          placeholder="Required with a GitHub login"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 text-sm"
        />
      </label>
      <label class="text-2xs text-muted">
        Initial password
        <input
          v-model="newUserPassword"
          type="password"
          autocomplete="new-password"
          placeholder="Optional"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 text-sm"
        />
      </label>
      <label class="text-2xs text-muted">
        Role
        <select v-model="newUserRole" class="mt-1 w-full rounded bg-input px-2 py-1.5 text-sm">
          <option value="user">User — normal Loom operation</option>
          <option value="admin">Admin — deployment configuration</option>
        </select>
      </label>
      <div class="md:col-span-2">
        <button
          class="btn-primary px-3 py-1.5 text-xs"
          :disabled="Boolean(busy) || !newUser.trim()"
          @click="add"
        >
          Approve user
        </button>
      </div>
    </div>
  </section>
</template>
