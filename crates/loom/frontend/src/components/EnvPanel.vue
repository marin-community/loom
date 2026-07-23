<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import * as api from '../api';
import type { EnvVar } from '../types';
import SettingsTableSection from './SettingsTableSection.vue';

// The default profile's environment variables. Other profiles have independent
// environment stores, and restricted profiles never inherit these values.
const vars = ref<EnvVar[]>([]);
// Per-name editable draft so an in-progress value edit survives a reload.
const drafts = ref<Record<string, string>>({});
const error = ref('');
const notice = ref('');
const busy = ref('');

const newName = ref('');
const newValue = ref('');

// A POSIX shell identifier, excluding loom's own reserved prefixes — what the
// server enforces, mirrored here so the Add button can disable on an obviously
// bad name (bad shape, or a name that would shadow loom's WEAVER_*/LOOM_ env)
// before the round-trip.
const nameOk = (n: string) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(n) && !/^(WEAVER_|LOOM_)/.test(n);
const newNameValid = computed(() => nameOk(newName.value.trim()));

function sync(list: EnvVar[]) {
  vars.value = list;
  // Preserve any in-progress edit for a name that still exists; only seed a
  // draft for newly-appeared names and drop drafts for names that are gone. This
  // keeps a refresh (after saving/deleting another row) from clobbering the row
  // you're typing in.
  const next: Record<string, string> = {};
  for (const v of list) {
    next[v.name] = v.name in drafts.value ? drafts.value[v.name] : v.value;
  }
  drafts.value = next;
}

async function load() {
  try {
    sync(await api.listEnv());
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

onMounted(load);

function dirty(v: EnvVar): boolean {
  return drafts.value[v.name] !== v.value;
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

const save = (v: EnvVar) =>
  act(v.name, async () => {
    sync(await api.setEnv(v.name, drafts.value[v.name] ?? ''));
    notice.value = `Saved ${v.name}.`;
  });

const remove = (v: EnvVar) =>
  act(v.name, async () => {
    if (!confirm(`Delete ${v.name}? New agent sessions will no longer see it.`)) return;
    sync(await api.deleteEnv(v.name));
    notice.value = `Deleted ${v.name}.`;
  });

const add = () =>
  act('+add', async () => {
    const name = newName.value.trim();
    if (!name || !newNameValid.value) return;
    sync(await api.setEnv(name, newValue.value));
    notice.value = `Added ${name}.`;
    newName.value = '';
    newValue.value = '';
  });
</script>

<template>
  <div>
    <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">
      Default profile environment
    </h2>
    <p class="mb-3 text-xs text-faint">
      Variables exported into new sessions that use the default profile. Other profiles use their
      own environment; restricted profiles do not inherit these values. Values on this screen are
      readable configuration, not a secret store. Changes apply to future sessions only.
    </p>

    <p v-if="error" class="mb-3 text-sm text-block">{{ error }}</p>
    <p v-if="notice" class="mb-3 text-sm text-accent">{{ notice }}</p>

    <SettingsTableSection
      :columns="['Name', 'Value', 'Actions']"
      grid-class="md:grid-cols-[minmax(11rem,14rem)_minmax(0,1fr)_auto]"
    >
      <div
        class="grid grid-cols-1 gap-2 border-b border-line px-3 py-2.5 md:grid-cols-[minmax(11rem,14rem)_minmax(0,1fr)_auto]"
      >
        <label class="flex flex-col gap-1 md:min-w-0">
          <span class="text-2xs text-muted md:sr-only">Name</span>
          <input
            v-model="newName"
            placeholder="e.g. GH_HOST"
            data-testid="env-name"
            class="rounded bg-input px-2 py-1 font-mono text-sm outline-none focus:ring-1 ring-accent"
            @keyup.enter="add"
          />
        </label>
        <label class="flex flex-col gap-1 md:min-w-0">
          <span class="text-2xs text-muted md:sr-only">Value</span>
          <input
            v-model="newValue"
            placeholder="(value)"
            data-testid="env-value"
            class="rounded bg-input px-2 py-1 font-mono text-sm outline-none focus:ring-1 ring-accent"
            @keyup.enter="add"
          />
        </label>
        <div class="flex items-center gap-2 md:justify-end">
          <button
            class="btn-primary px-2.5 py-1 text-xs"
            :disabled="busy === '+add' || !newNameValid"
            data-testid="env-add"
            @click="add"
          >
            Add
          </button>
        </div>
      </div>
      <p
        v-if="newName.trim() && !newNameValid"
        class="border-b border-line px-3 py-2 text-2xs text-block"
      >
        A name must start with a letter or underscore and contain only letters, digits, and
        underscores, and cannot start with <code>WEAVER_</code> or <code>LOOM_</code>.
      </p>

      <div
        v-for="v in vars"
        :key="v.name"
        class="grid grid-cols-1 gap-2 border-b border-line px-3 py-2.5 last:border-0 md:grid-cols-[minmax(11rem,14rem)_minmax(0,1fr)_auto]"
        data-testid="env-row"
      >
        <code class="truncate font-mono text-sm" :title="v.name">{{ v.name }}</code>
        <div class="flex flex-col gap-1 md:min-w-0">
          <span class="text-2xs text-muted md:sr-only">Value</span>
          <input
            v-model="drafts[v.name]"
            class="rounded bg-input px-2 py-1 font-mono text-sm outline-none focus:ring-1 ring-accent"
            spellcheck="false"
            autocapitalize="off"
            autocomplete="off"
            @keyup.enter="save(v)"
          />
        </div>
        <div class="flex items-center gap-2 md:justify-end">
          <button
            class="btn-primary px-2.5 py-1 text-xs"
            :disabled="busy === v.name || !dirty(v)"
            @click="save(v)"
          >
            Save
          </button>
          <button
            class="btn-secondary px-2.5 py-1 text-xs"
            :disabled="busy === v.name"
            data-testid="env-delete"
            @click="remove(v)"
          >
            Delete
          </button>
        </div>
      </div>
      <p v-if="!vars.length" class="px-3 py-2.5 text-sm text-muted">No variables yet.</p>
    </SettingsTableSection>
  </div>
</template>
