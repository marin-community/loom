<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { createCustomMcp, deleteCustomMcp, getMcpRegistry, updateCustomMcp } from '../api';
import type { CustomMcp, CustomMcpInput, McpRegistry } from '../types';
import { confirmAction } from '../lib/confirmation';
import ToggleSwitch from './ToggleSwitch.vue';

const registry = ref<McpRegistry | null>(null);
const editing = ref<string | null>(null);
const draft = ref<CustomMcpInput>(blank());
const busy = ref(false);
const error = ref('');

const custom = computed(() => registry.value?.custom_servers ?? []);
const isNew = computed(() => editing.value === '');

function blank(): CustomMcpInput {
  return {
    identity: '',
    label: '',
    description: '',
    source:
      '# /// script\n# dependencies = ["mcp"]\n# ///\n\nfrom mcp.server.fastmcp import FastMCP\n\nserver = FastMCP("custom")\n\n@server.tool()\ndef example(value: str) -> str:\n    """Replace this example tool."""\n    return value\n\nserver.run()\n',
    test_source: '',
    enabled: true,
  };
}

async function load() {
  registry.value = await getMcpRegistry();
}

function add() {
  editing.value = '';
  draft.value = blank();
  error.value = '';
}

function edit(server: CustomMcp) {
  editing.value = server.identity;
  draft.value = {
    identity: server.identity,
    label: server.label,
    description: server.description,
    source: server.source,
    test_source: server.test_source,
    enabled: server.enabled,
  };
  error.value = '';
}

function cancel() {
  editing.value = null;
  error.value = '';
}

async function save() {
  busy.value = true;
  error.value = '';
  try {
    const saved = isNew.value
      ? await createCustomMcp(draft.value)
      : await updateCustomMcp(editing.value as string, draft.value);
    await load();
    if (saved.validation_state === 'ready') editing.value = null;
    else {
      editing.value = saved.identity;
      error.value = saved.validation_message || 'Validation failed.';
    }
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function remove(server: CustomMcp) {
  await confirmAction({
    title: `Delete custom MCP "${server.identity}"?`,
    description:
      'New profile revisions cannot select it. Existing sessions keep their immutable snapshots.',
    confirmLabel: 'Delete MCP',
    danger: true,
    action: async () => {
      busy.value = true;
      error.value = '';
      try {
        await deleteCustomMcp(server.identity);
        await load();
        if (editing.value === server.identity) editing.value = null;
      } finally {
        busy.value = false;
      }
    },
  });
}

onMounted(() => void load().catch((e) => (error.value = (e as Error).message)));
</script>

<template>
  <section data-testid="mcp-panel" class="rounded-md border border-line bg-surface">
    <header class="flex items-center gap-3 border-b border-line px-3 py-2">
      <div>
        <h3 class="text-sm font-semibold">MCP servers</h3>
        <p class="text-xs text-muted">
          Builtins are trusted Loom adapters. Custom Python scripts are revisioned, run through
          <code>uv</code>, and tested over real MCP stdio before profiles can use them.
        </p>
      </div>
      <button class="btn-secondary ml-auto px-2.5 py-1 text-xs" @click="add">Add custom MCP</button>
    </header>

    <p v-if="error" class="border-b border-line px-3 py-2 text-xs text-block">{{ error }}</p>

    <div class="border-b border-line px-3 py-2">
      <h4 class="mb-1 text-xs font-medium">Builtins</h4>
      <ul class="space-y-1 text-xs text-muted">
        <li v-for="set in registry?.capability_sets ?? []" :key="set.name">
          <code>{{ set.name }}</code>
          <span class="ml-1 rounded bg-input px-1 py-0.5">{{ set.group }}</span>
          — {{ set.description }}
        </li>
      </ul>
    </div>

    <ul v-if="custom.length" class="divide-y divide-line">
      <li v-for="server in custom" :key="server.identity" class="flex items-center gap-2 px-3 py-2">
        <div class="min-w-0">
          <div class="flex flex-wrap items-center gap-1.5">
            <span class="text-sm font-medium">{{ server.label }}</span>
            <code class="text-2xs text-faint">{{ server.identity }}</code>
            <span
              class="rounded px-1 py-0.5 text-2xs"
              :class="
                server.validation_state === 'ready'
                  ? 'bg-ok-soft text-ok'
                  : 'bg-block-soft text-block'
              "
            >
              {{ server.validation_state }} · r{{ server.revision }}
            </span>
            <span v-if="!server.enabled" class="text-2xs text-faint">disabled</span>
          </div>
          <p class="truncate text-xs text-muted">
            {{ server.description || server.tools.join(', ') || server.validation_message }}
          </p>
        </div>
        <div class="ml-auto flex gap-1">
          <button class="btn-secondary px-2 py-1 text-xs" @click="edit(server)">Edit</button>
          <button class="btn-secondary px-2 py-1 text-xs text-block" @click="remove(server)">
            Delete
          </button>
        </div>
      </li>
    </ul>
    <p v-else class="px-3 py-2 text-xs text-muted">No custom MCP servers.</p>

    <form v-if="editing !== null" class="space-y-3 border-t border-line p-3" @submit.prevent="save">
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="text-xs">
          Identity
          <input
            v-model="draft.identity"
            :disabled="!isNew"
            placeholder="/engineering/search/docs"
            class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono"
          />
        </label>
        <label class="text-xs">
          Label
          <input v-model="draft.label" class="mt-1 w-full rounded bg-input px-2 py-1.5" />
        </label>
      </div>
      <label class="block text-xs">
        Description
        <input v-model="draft.description" class="mt-1 w-full rounded bg-input px-2 py-1.5" />
      </label>
      <label class="block text-xs">
        Python MCP source (PEP 723)
        <textarea
          v-model="draft.source"
          rows="18"
          spellcheck="false"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-xs"
        ></textarea>
      </label>
      <label class="block text-xs">
        Tests (optional uv script; <code>LOOM_MCP_SOURCE</code> names the source file)
        <textarea
          v-model="draft.test_source"
          rows="7"
          spellcheck="false"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-xs"
        ></textarea>
      </label>
      <div class="flex items-center gap-2">
        <ToggleSwitch :model-value="draft.enabled" @update:model-value="draft.enabled = $event" />
        <span class="text-xs text-muted">Enabled for ordinary profile selection</span>
      </div>
      <div class="flex gap-2">
        <button class="btn-primary px-2.5 py-1 text-xs" :disabled="busy">Save and validate</button>
        <button type="button" class="btn-secondary px-2.5 py-1 text-xs" @click="cancel">
          Cancel
        </button>
      </div>
    </form>
  </section>
</template>
