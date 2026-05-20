<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { get, patch } from '../api';
import type { SettingView } from '../types';

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
    settings.value = res.settings;
    drafts.value = Object.fromEntries(res.settings.map((s) => [s.key, s.value]));
    error.value = '';
  } catch (e) {
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
  <div>
    <div class="flex items-center gap-3 mb-1">
      <router-link to="/" class="text-neutral-500 hover:text-neutral-300 text-sm"
        >← all</router-link
      >
      <h1 class="text-xl font-semibold">Settings</h1>
    </div>
    <p class="text-xs text-neutral-600 mb-4">
      Stored in the weaver database and shared by the server and CLI
      (<code>weaver config</code>).
    </p>

    <p v-if="error" class="mb-3 text-sm text-red-400">{{ error }}</p>
    <p v-if="notice" class="mb-3 text-sm text-emerald-400">{{ notice }}</p>

    <div v-for="g in groups" :key="g.name" class="mb-6">
      <h2 class="text-sm font-semibold text-neutral-400 uppercase tracking-wide mb-2">
        {{ g.name }}
      </h2>
      <div class="space-y-3">
        <section
          v-for="s in g.items"
          :key="s.key"
          class="rounded border border-neutral-800 bg-neutral-900 p-4"
        >
          <div class="flex items-center justify-between gap-2 mb-1">
            <label :for="s.key" class="text-sm font-medium">{{ s.label }}</label>
            <span class="font-mono text-xs text-neutral-600">{{ s.key }}</span>
          </div>
          <p class="text-xs text-neutral-500 mb-2">{{ s.description }}</p>

          <div class="flex items-center gap-2">
            <label v-if="s.kind === 'bool'" class="flex items-center gap-2 text-sm">
              <input
                :id="s.key"
                type="checkbox"
                :checked="drafts[s.key] === 'true'"
                class="accent-emerald-600"
                @change="
                  drafts[s.key] = ($event.target as HTMLInputElement).checked
                    ? 'true'
                    : 'false'
                "
              />
              <span class="text-neutral-400">{{
                drafts[s.key] === 'true' ? 'Enabled' : 'Disabled'
              }}</span>
            </label>
            <input
              v-else
              :id="s.key"
              v-model="drafts[s.key]"
              :type="s.kind === 'int' ? 'number' : 'text'"
              :placeholder="s.default || '(empty)'"
              class="flex-1 rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600"
              :class="{ 'font-mono': s.kind === 'string' }"
            />
            <button
              class="rounded bg-emerald-700 hover:bg-emerald-600 px-3 py-1.5 text-sm disabled:opacity-40"
              :disabled="busy === s.key || !dirty(s)"
              @click="save(s)"
            >
              Save
            </button>
            <button
              class="rounded bg-neutral-700 hover:bg-neutral-600 px-3 py-1.5 text-sm disabled:opacity-40"
              :disabled="busy === s.key || s.is_default"
              :title="`Reset to default: ${s.default || '(empty)'}`"
              @click="reset(s)"
            >
              Reset
            </button>
          </div>
          <p class="mt-1.5 text-xs text-neutral-600">
            <span v-if="s.is_default">Using the default</span>
            <span v-else>Customized · default is</span>
            <code>{{ s.default || '(empty)' }}</code>
          </p>
        </section>
      </div>
    </div>

    <p v-if="!settings.length && !error" class="text-neutral-500 text-sm">Loading…</p>
  </div>
</template>
