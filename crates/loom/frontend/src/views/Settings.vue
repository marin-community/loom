<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { get, patch } from '../api';
import type { SettingView } from '../types';
import ToggleSwitch from '../components/ToggleSwitch.vue';
import TokensPanel from '../components/TokensPanel.vue';
import AccountPanel from '../components/AccountPanel.vue';

// In-page tabs: the setting registry (General), API tokens, and the account /
// access surface (password, approved users, GitHub sign-in config).
type Tab = 'general' | 'tokens' | 'account';
const tabs: { id: Tab; label: string }[] = [
  { id: 'general', label: 'General' },
  { id: 'tokens', label: 'Tokens' },
  { id: 'account', label: 'Account' },
];
const tab = ref<Tab>('general');

// The server's canonical reply for both GET and PATCH /api/settings.
interface SettingsEnvelope {
  settings: SettingView[];
}

const settings = ref<SettingView[]>([]);
// Per-key editable draft, keyed by setting key.
const drafts = ref<Record<string, string>>({});
const error = ref('');
const notice = ref('');
const busy = ref('');

// Settings grouped by their `group`, preserving registry order.
const groups = computed(() => {
  const out: { name: string; items: SettingView[] }[] = [];
  for (const s of settings.value) {
    let g = out.find((x) => x.name === s.group);
    if (!g) {
      g = { name: s.group, items: [] };
      out.push(g);
    }
    g.items.push(s);
  }
  return out;
});

async function load() {
  try {
    const res = (await get('/settings')) as SettingsEnvelope;
    // Validate the shape before touching reactive state. A stale server — one
    // built before the settings endpoint existed — answers `{}`; assigning its
    // missing `settings` to the ref would crash the render (the `groups`
    // computed iterates it) and leave a blank page instead of this message.
    if (!Array.isArray(res?.settings)) {
      throw new Error('Unexpected /api/settings response — the server may be out of date.');
    }
    settings.value = res.settings;
    drafts.value = Object.fromEntries(res.settings.map((s) => [s.key, s.value]));
    error.value = '';
  } catch (e) {
    settings.value = [];
    error.value = (e as Error).message;
  }
}

function dirty(s: SettingView): boolean {
  return drafts.value[s.key] !== s.value;
}

async function act(key: string, fn: () => Promise<void>) {
  busy.value = key;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = '';
  }
}

// Adopt a PATCH reply: refresh the canonical values, and resync only the
// changed key's draft so other in-progress edits are left untouched.
function adopt(res: SettingsEnvelope, changedKey: string) {
  settings.value = res.settings;
  const changed = res.settings.find((s) => s.key === changedKey);
  if (changed) drafts.value[changedKey] = changed.value;
}

// PATCH a single key — a value to set it, null to reset it to its default.
const apply = (s: SettingView, value: string | null, verb: string) =>
  act(s.key, async () => {
    const res = (await patch('/settings', { [s.key]: value })) as SettingsEnvelope;
    adopt(res, s.key);
    notice.value = `${verb} ${s.label}.`;
  });

const save = (s: SettingView) => apply(s, drafts.value[s.key] ?? '', 'Saved');
const reset = (s: SettingView) => apply(s, null, 'Reset');

onMounted(load);
</script>

<template>
  <div class="max-w-3xl px-5 py-3">
    <div class="mb-2 flex min-h-7 items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Settings</h1>
    </div>

    <!-- Tab bar: General (registry), Tokens, Account. -->
    <div class="mb-4 flex gap-1 border-b border-line">
      <button
        v-for="t in tabs"
        :key="t.id"
        :data-testid="`settings-tab-${t.id}`"
        class="-mb-px border-b-2 px-3 py-1.5 text-sm transition-colors"
        :class="
          tab === t.id
            ? 'border-accent text-fg'
            : 'border-transparent text-muted hover:text-fg'
        "
        @click="tab = t.id"
      >
        {{ t.label }}
      </button>
    </div>

    <TokensPanel v-if="tab === 'tokens'" />
    <AccountPanel v-else-if="tab === 'account'" />

    <div v-else>
      <p class="text-xs text-faint mb-3">
        Stored in the weaver database and shared by the server and CLI
        (<code>weaver config</code>).
      </p>

      <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>
      <p v-if="notice" class="mb-3 text-sm text-accent">{{ notice }}</p>

      <!-- One bordered panel per group, hairline-divided rows — the same list
           anatomy as the fleet/issue boards, instead of free-floating cards. -->
      <div v-for="g in groups" :key="g.name" class="mb-5">
      <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
        {{ g.name }}
      </h2>
      <div class="overflow-hidden rounded-md border border-line bg-surface">
        <section
          v-for="s in g.items"
          :key="s.key"
          class="border-b border-line px-3 py-2.5 last:border-0"
        >
          <div class="flex items-center justify-between gap-2">
            <label :for="s.key" class="text-sm font-medium">{{ s.label }}</label>
            <span class="font-mono text-2xs text-faint">{{ s.key }}</span>
          </div>
          <p class="text-xs text-muted mt-0.5 mb-2">{{ s.description }}</p>

          <div class="flex items-center gap-2">
            <div v-if="s.kind === 'bool'" class="flex flex-1 items-center gap-2 text-sm">
              <ToggleSwitch
                :id="s.key"
                :model-value="drafts[s.key] === 'true'"
                @update:model-value="drafts[s.key] = $event ? 'true' : 'false'"
              />
              <span class="text-xs text-muted">{{
                drafts[s.key] === 'true' ? 'Enabled' : 'Disabled'
              }}</span>
            </div>
            <select
              v-else-if="s.kind === 'enum'"
              :id="s.key"
              v-model="drafts[s.key]"
              class="flex-1 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
            >
              <option v-for="opt in s.options" :key="opt" :value="opt">{{ opt }}</option>
            </select>
            <input
              v-else
              :id="s.key"
              v-model="drafts[s.key]"
              :type="s.kind === 'int' ? 'number' : 'text'"
              :placeholder="s.default || '(empty)'"
              class="flex-1 rounded bg-input px-2 py-1 text-sm outline-none focus:ring-1 ring-accent"
              :class="{ 'font-mono': s.kind === 'string' }"
            />
            <button
              class="btn-primary px-2.5 py-1 text-xs"
              :disabled="busy === s.key || !dirty(s)"
              @click="save(s)"
            >
              Save
            </button>
            <button
              class="btn-secondary px-2.5 py-1 text-xs"
              :disabled="busy === s.key || s.is_default"
              :title="`Reset to default: ${s.default || '(empty)'}`"
              @click="reset(s)"
            >
              Reset
            </button>
          </div>
          <p class="mt-1.5 text-2xs text-faint">
            <span v-if="s.is_default">Using the default:</span>
            <span v-else>Customized · default is</span>
            <code class="ml-1 font-mono">{{ s.default || '(empty)' }}</code>
          </p>
        </section>
      </div>
      </div>

      <p v-if="!settings.length && !error" class="text-sm text-muted">Loading…</p>
    </div>
  </div>
</template>
