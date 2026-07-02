<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useRouter } from 'vue-router';
import * as api from '../api';
import { me, doLogout } from '../auth';
import type { User, GithubConfig } from '../types';

// Account + access management: who you are, your password, the approved-user
// allowlist, and the single GitHub App that backs loom — its OAuth client powers
// "Continue with GitHub", and the same App drives the `@loom` trigger.
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
// A personal fine-grained PAT, injected as GH_TOKEN into the sessions this user
// launches, so their agents' `git push` / `gh` act as them (not the shared
// ambient token). Write-only: we render only whether it's set, never the value.
const PAT_CREATE_URL = 'https://github.com/settings/personal-access-tokens/new';
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
    ok('GitHub token saved — your new sessions will act as you.');
  } catch (e) {
    fail(e);
  } finally {
    busy.value = false;
  }
}

async function clearMyGithubToken() {
  if (!confirm('Remove your GitHub token? Your sessions will fall back to the shared token.'))
    return;
  busy.value = true;
  try {
    await api.deleteMyGithubToken();
    ghTokenStatus.value = { set: false, updated_at: null };
    ok('GitHub token removed.');
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

// -- GitHub App -------------------------------------------------------------
// One GitHub App backs loom: its OAuth client id/secret power "Continue with
// GitHub", and the same App's id + private key power the `@loom` trigger. The
// usual way to set it up is `loom setup github-app`; the id/secret below stay
// editable for the manual path (or a login-only classic OAuth app).
const gh = ref<GithubConfig | null>(null);
const ghClientId = ref('');
const ghClientSecret = ref('');

// The App's public GitHub page, when we know its slug (recorded by
// `loom setup github-app`). A hand-configured App has an id but no slug.
const appUrl = computed(() =>
  gh.value?.app_slug ? `https://github.com/apps/${gh.value.app_slug}` : '',
);

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
  loadMyGithubToken();
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

    <!-- Your GitHub token -->
    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        Your GitHub token
      </h2>
      <div class="rounded-md border border-line bg-surface px-3 py-2.5">
        <p class="text-xs text-muted mb-2">
          A fine-grained token your sessions use for <code class="font-mono">git push</code> and
          <code class="font-mono">gh</code>, so your agents act as you.
          <a class="text-accent underline" :href="PAT_CREATE_URL" target="_blank" rel="noopener">
            Create one</a>
          with <span class="font-medium">Contents</span> and
          <span class="font-medium">Pull requests</span> read/write on the repos you work in.
          <span :class="ghTokenStatus?.set ? 'text-accent' : 'text-faint'">
            {{ ghTokenStatus?.set ? 'Set.' : 'Not set — sessions use the shared token.' }}
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

    <!-- GitHub App -->
    <section>
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        GitHub App
      </h2>
      <div class="rounded-md border border-line bg-surface px-3 py-2.5">
        <!-- App identity: one App powers both sign-in and the @loom trigger. -->
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
            One GitHub App powers both sign-in and the <code class="font-mono">@loom</code>
            trigger. Manage it with <code class="font-mono">loom setup github-app</code>.
          </p>
        </div>
        <p v-else class="text-xs text-muted mb-2">
          No GitHub App configured. Run
          <code class="font-mono">loom setup github-app --base-url &lt;your loom URL&gt;</code>
          to register one — it wires up sign-in and the <code class="font-mono">@loom</code>
          trigger in a single step. You can also paste sign-in credentials manually below.
        </p>

        <!-- Sign-in (OAuth) credentials: the App's OAuth client, editable for
             the manual path or a login-only classic OAuth app. -->
        <p class="text-2xs font-semibold uppercase tracking-wider text-muted mt-3 mb-1">
          Sign-in credentials
        </p>
        <p class="text-xs text-muted mb-2">
          <template v-if="gh?.app_configured">The same App's</template>
          <template v-else>The</template>
          OAuth client, with callback
          <code class="font-mono">{{ gh?.callback_path }}</code>. Powers "Continue with GitHub".
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
      <p class="text-2xs text-faint mb-1.5">
        Everyone allowed near loom. An approved user can sign in here, and — if their
        GitHub login is on file — trigger a session by commenting
        <code class="font-mono">@loom</code> on a GitHub PR or issue.
      </p>
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
