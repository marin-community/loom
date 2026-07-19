<script setup lang="ts">
import { ref, computed } from 'vue';
import type { CustomAgent, CustomAgentInput } from '../types';
import { createCustomAgent, updateCustomAgent, deleteCustomAgent } from '../api';
import ToggleSwitch from './ToggleSwitch.vue';

const props = defineProps<{ agents: CustomAgent[] }>();
const emit = defineEmits<{ reload: [] }>();

// `null` = the form is closed; a string = editing that agent by name; '' = adding
// a new one. `draft` holds the in-progress definition.
const editing = ref<string | null>(null);
const draft = ref<CustomAgentInput>(blank());
const busy = ref(false);
const error = ref('');

function blank(): CustomAgentInput {
  return { name: '', label: '', setup: '', launch: '', resume: '', reports_status: false };
}

const isNew = computed(() => editing.value === '');
const open = computed(() => editing.value !== null);

function startAdd() {
  editing.value = '';
  draft.value = blank();
  error.value = '';
}

function startEdit(agent: CustomAgent) {
  editing.value = agent.name;
  draft.value = {
    name: agent.name,
    label: agent.label,
    setup: agent.setup,
    launch: agent.launch,
    resume: agent.resume,
    reports_status: agent.reports_status,
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
    if (isNew.value) {
      await createCustomAgent(draft.value);
    } else {
      await updateCustomAgent(editing.value as string, draft.value);
    }
    editing.value = null;
    emit('reload');
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function remove(agent: CustomAgent) {
  if (!window.confirm(`Delete the custom agent "${agent.label}" (${agent.name})?`)) return;
  busy.value = true;
  error.value = '';
  try {
    await deleteCustomAgent(agent.name);
    if (editing.value === agent.name) editing.value = null;
    emit('reload');
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <section class="rounded-md border border-line bg-surface">
    <div class="flex flex-wrap items-center gap-3 border-b border-line px-3 py-2">
      <div class="min-w-0">
        <h3 class="text-sm font-semibold">Custom agents</h3>
        <p class="text-xs text-muted">
          Wire up an agent loom doesn't ship by naming the shell commands it runs at each launch
          stage. It then appears in the agent picker beside Claude and Codex.
        </p>
      </div>
      <button
        v-if="!open || !isNew"
        class="btn-secondary ml-auto px-2.5 py-1 text-xs"
        :disabled="busy"
        data-testid="custom-agent-add"
        @click="startAdd"
      >
        Add agent
      </button>
    </div>

    <p v-if="error" class="border-b border-line px-3 py-2 text-xs text-block">{{ error }}</p>

    <!-- Existing custom agents -->
    <ul v-if="props.agents.length" class="divide-y divide-line">
      <li
        v-for="agent in props.agents"
        :key="agent.name"
        class="flex flex-wrap items-center gap-2 px-3 py-2"
      >
        <div class="min-w-0">
          <div class="flex items-center gap-2">
            <span class="text-sm font-medium">{{ agent.label }}</span>
            <code class="font-mono text-2xs text-faint">{{ agent.name }}</code>
            <span
              v-if="agent.reports_status"
              class="rounded bg-ok-soft px-1.5 py-0.5 text-2xs text-ok"
            >
              hooks
            </span>
          </div>
          <p class="truncate font-mono text-2xs text-muted">
            {{ agent.launch || agent.setup || '(bare shell)' }}
          </p>
        </div>
        <div class="ml-auto flex items-center gap-1.5">
          <button
            class="btn-secondary px-2 py-1 text-xs"
            :disabled="busy"
            @click="startEdit(agent)"
          >
            Edit
          </button>
          <button
            class="btn-secondary px-2 py-1 text-xs text-block"
            :disabled="busy"
            @click="remove(agent)"
          >
            Delete
          </button>
        </div>
      </li>
    </ul>
    <p v-else-if="!open" class="px-3 py-3 text-xs text-muted">No custom agents yet.</p>

    <!-- Add / edit form -->
    <form v-if="open" class="space-y-3 border-t border-line px-3 py-3" @submit.prevent="save">
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="block">
          <span class="text-2xs font-semibold uppercase tracking-wider text-muted">Name</span>
          <input
            v-model="draft.name"
            :disabled="!isNew"
            placeholder="aider"
            data-testid="custom-agent-name"
            class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1 disabled:opacity-60"
          />
          <span class="mt-0.5 block text-2xs text-faint">
            Id used in the agent list. Letters, digits, hyphens; fixed once created.
          </span>
        </label>
        <label class="block">
          <span class="text-2xs font-semibold uppercase tracking-wider text-muted">Label</span>
          <input
            v-model="draft.label"
            placeholder="Aider"
            data-testid="custom-agent-label"
            class="mt-1 w-full rounded bg-input px-2 py-1.5 text-sm outline-none ring-accent focus:ring-1"
          />
        </label>
      </div>

      <label class="block">
        <span class="text-2xs font-semibold uppercase tracking-wider text-muted">
          Setup / install hooks
        </span>
        <textarea
          v-model="draft.setup"
          rows="2"
          placeholder="Runs in the worktree before launch — e.g. write a config or install status hooks. Optional."
          class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-xs outline-none ring-accent focus:ring-1"
        ></textarea>
      </label>

      <label class="block">
        <span class="text-2xs font-semibold uppercase tracking-wider text-muted">
          Launch command
        </span>
        <input
          v-model="draft.launch"
          placeholder="aider --message"
          data-testid="custom-agent-launch"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
        />
        <span class="mt-0.5 block text-2xs text-faint">
          The fresh-session command; the goal is appended as a quoted argument. Leave blank for a
          bare shell.
        </span>
      </label>

      <label class="block">
        <span class="text-2xs font-semibold uppercase tracking-wider text-muted">
          Resume command
        </span>
        <input
          v-model="draft.resume"
          placeholder="aider --continue"
          class="mt-1 w-full rounded bg-input px-2 py-1.5 font-mono text-sm outline-none ring-accent focus:ring-1"
        />
        <span class="mt-0.5 block text-2xs text-faint">
          Used when resuming an existing worktree (no goal). Blank reuses the launch command.
        </span>
      </label>

      <div class="flex items-center gap-2">
        <ToggleSwitch
          :model-value="draft.reports_status"
          @update:model-value="draft.reports_status = $event"
        />
        <span class="text-xs text-muted">
          Reports status via weaver hooks
          <span class="text-faint">
            — off: the session is <code>running</code> immediately, with no live working/idle state.
          </span>
        </span>
      </div>

      <div class="flex items-center gap-2">
        <button
          type="submit"
          class="btn-primary px-2.5 py-1 text-xs disabled:opacity-50"
          :disabled="busy"
          data-testid="custom-agent-save"
        >
          {{ isNew ? 'Add agent' : 'Save' }}
        </button>
        <button
          type="button"
          class="btn-secondary px-2.5 py-1 text-xs"
          :disabled="busy"
          @click="cancel"
        >
          Cancel
        </button>
      </div>
    </form>
  </section>
</template>
