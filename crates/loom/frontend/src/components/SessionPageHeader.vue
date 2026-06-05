<script setup lang="ts">
import { ref, nextTick } from 'vue';
import type { Session } from '../types';
import { messageOf } from '../lib/sessionState';
import StatusBadge from './StatusBadge.vue';
import SessionDetailsPopover from './SessionDetailsPopover.vue';

// The detail page header: a compact answer-zone.
//   row 1  ← all · title (inline rename) · lifecycle badge
//   row 2  the agent's current-state message as prose (the point of the page)
//   row 3  one quiet repo/branch · agent line + a ⌄ details popover holding the
//          low-frequency machine metadata (id, base, tmux, worktree, github)
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ rename: [string] }>();

const showDetails = ref(false);

// Inline title rename — the title lives only here, no separate edit box. Click
// the ✎ to edit; Enter/blur commits, Esc cancels. Title is the one branch field
// a human authors; goal and status are agent-authored and read-only elsewhere.
const editing = ref(false);
const draft = ref('');
const inputEl = ref<HTMLInputElement | null>(null);

function current(): string {
  return props.ws.branch.title || props.ws.branch.name;
}

async function startEdit() {
  draft.value = current();
  editing.value = true;
  await nextTick();
  inputEl.value?.focus();
  inputEl.value?.select();
}

function commit() {
  if (!editing.value) return;
  editing.value = false;
  const next = draft.value.trim();
  if (next && next !== current()) emit('rename', next);
}

function cancel() {
  editing.value = false;
}

// The short repo label is the last path segment of the worktree's repo root.
function repoName(p: string): string {
  return p.replace(/\/+$/, '').split('/').pop() || p;
}
</script>

<template>
  <header class="mb-3">
    <!-- Row 1 — back link, title (inline rename), badges -->
    <div class="flex items-center gap-3">
      <router-link to="/" class="text-sm text-muted hover:text-fg">← all</router-link>
      <input
        v-if="editing"
        ref="inputEl"
        v-model="draft"
        class="min-w-0 flex-1 rounded bg-input px-2 py-1 text-lg font-semibold outline-none focus:ring-1 ring-accent"
        @keydown.enter.prevent="commit"
        @keydown.esc.prevent="cancel"
        @blur="commit"
      />
      <div v-else class="group flex min-w-0 items-center gap-1.5">
        <h1 class="min-w-0 truncate text-lg font-semibold tracking-tight">
          {{ ws.branch.title || ws.branch.name }}
        </h1>
        <button
          type="button"
          class="shrink-0 text-xs text-faint opacity-0 transition-opacity hover:text-fg group-hover:opacity-100"
          title="Rename"
          @click="startEdit"
        >
          ✎
        </button>
      </div>
      <!-- Lifecycle only. The agent's attention signal lives in the status
           strip below — one place, and it carries the acknowledge control — so
           it is not duplicated as a badge here. -->
      <StatusBadge class="ml-auto shrink-0" :status="ws.status" />
    </div>

    <!-- Row 2 — the current-state headline (the agent's "where am I"). Full
         foreground — it's the point of the page, not chrome. -->
    <p
      v-if="messageOf(ws)"
      class="mt-1 line-clamp-2 text-sm leading-snug text-fg"
      data-testid="status-message"
    >
      {{ messageOf(ws) }}
    </p>
    <p v-else class="mt-1 text-sm text-faint">
      No status yet — agent hasn't run <code>weaver set-status</code>.
    </p>

    <!-- Row 3 — one quiet meta line: repo/branch · agent, with everything else
         (id, base, tmux, worktree, github) tucked behind ⌄ details. -->
    <div class="mt-2 flex items-center gap-2 text-xs">
      <span class="min-w-0 truncate font-mono text-muted">
        {{ repoName(ws.branch.repo_root) }}/{{ ws.branch.name }}
      </span>
      <span class="text-faint">·</span>
      <span class="text-muted">
        {{ ws.agent_kind }}<template v-if="ws.model"> · {{ ws.model }}</template>
      </span>
      <div class="relative ml-auto shrink-0">
        <button
          type="button"
          class="text-muted hover:text-fg"
          @click="showDetails = !showDetails"
        >
          ⌄ details
        </button>
        <SessionDetailsPopover :ws="ws" v-model:open="showDetails" />
      </div>
    </div>
  </header>
</template>
