<script setup lang="ts">
import { ref, computed, nextTick } from 'vue';
import type { Session } from '../types';
import { messageOf, conversationState, levelOf, TONE_TEXT } from '../lib/sessionState';
import { timeAgo } from '../lib/time';
import { useSessionActions } from '../lib/sessionActions';
import StatusBadge from './StatusBadge.vue';
import SessionDetailsPopover from './SessionDetailsPopover.vue';
import GithubStatus from './GithubStatus.vue';

// The session page header — one compact chrome block shared by both the detail
// view and the file browser, so the "where am I / what is this" context never
// vanishes when you cross into Files.
//
//   row 1  ← all · title (inline rename) · [attention chip + Mark OK]ⁱ
//           · lifecycle badge · ⌄ details menu
//   row 2  the agent's current-state message as prose (the point of the page)
//   row 3  repo/branch · agent · PR link · the quiet conversation-state + freshness
//
// The old full-width "▶ Working … last activity" strip is gone: when the agent
// is calm, its state is a quiet note on row 3; when it raises attention, the
// whole header takes the amber/red wash + a Mark OK control — one signal, in the
// one place you already look. ⁱ shown only when attention is actually raised.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ reload: [] }>();

const actions = useSessionActions(
  () => props.ws.id,
  () => emit('reload'),
);
const { busy, notice, error, rename, acknowledge, adopt, archive, remove } = actions;

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
  if (next && next !== current()) rename(next);
}

function cancel() {
  editing.value = false;
}

// The short repo label is the last path segment of the worktree's repo root.
function repoName(p: string): string {
  return p.replace(/\/+$/, '').split('/').pop() || p;
}

// Derived conversation state (glyph + label + tone) and the attention wash.
const conv = computed(() => conversationState(props.ws));
const toneClass = computed(() => TONE_TEXT[conv.value.tone]);
const lastActivity = computed(() => timeAgo(props.ws.last_activity_at));
const ackable = computed(() => levelOf(props.ws) !== 'ok');
// Only true attention/blocked elevates to the loud header treatment.
const loud = computed(() => conv.value.tone === 'block' || conv.value.tone === 'attn');
const washClass = computed(() =>
  conv.value.tone === 'block'
    ? 'border-block-line bg-block-soft'
    : conv.value.tone === 'attn'
      ? 'border-attn-line bg-attn-soft'
      : 'border-transparent',
);
</script>

<template>
  <header
    class="mb-3 rounded-r border-l-2 pl-3 pr-1 py-1.5 transition-colors"
    :class="[washClass, loud ? 'pulse-attention' : '']"
  >
    <!-- Row 1 — nav, title (inline rename), attention + lifecycle controls -->
    <div class="flex items-center gap-3">
      <router-link to="/" class="shrink-0 text-sm text-muted hover:text-fg">← all</router-link>
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

      <div class="ml-auto flex shrink-0 items-center gap-2">
        <!-- The loud attention signal, inline: chip + the one human control.
             Only present when the agent has actually raised attention. -->
        <template v-if="loud">
          <span
            data-testid="conversation-state"
            class="inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs font-semibold"
            :class="conv.tone === 'block' ? 'bg-block text-block-fg' : 'bg-attn text-attn-fg'"
          >
            {{ conv.glyph }} {{ conv.label }}
          </span>
          <button
            v-if="ackable"
            type="button"
            data-testid="acknowledge"
            class="rounded bg-surface px-2.5 py-1 text-xs font-semibold text-fg shadow-sm ring-1 ring-inset ring-line hover:bg-subtle"
            :disabled="busy === 'acknowledge'"
            @click="acknowledge"
          >
            Mark OK ✓
          </button>
        </template>

        <StatusBadge :status="ws.status" />

        <!-- ⌄ details — identity metadata + the lifecycle actions. -->
        <div class="relative">
          <button
            type="button"
            class="rounded px-1.5 py-1 text-xs text-muted hover:bg-subtle hover:text-fg"
            @click="showDetails = !showDetails"
          >
            ⌄ details
          </button>
          <SessionDetailsPopover :ws="ws" v-model:open="showDetails">
            <template #actions>
              <div class="flex flex-wrap items-center gap-2">
                <button
                  v-if="ws.status === 'orphaned'"
                  class="rounded bg-subtle px-3 py-1.5 text-xs text-accent ring-1 ring-inset ring-accent/30 hover:bg-subtle-hover"
                  :disabled="busy === 'adopt'"
                  @click="adopt"
                >
                  {{ busy === 'adopt' ? 'Adopting…' : 'Adopt' }}
                </button>
                <button
                  v-if="ws.status !== 'archived'"
                  class="rounded bg-subtle px-3 py-1.5 text-xs text-fg hover:bg-subtle-hover"
                  :disabled="busy === 'archive'"
                  @click="archive"
                >
                  {{ busy === 'archive' ? 'Archiving…' : 'Archive' }}
                </button>
                <button
                  class="ml-auto rounded bg-transparent px-3 py-1.5 text-xs text-block ring-1 ring-inset ring-block-line hover:bg-block-soft"
                  :disabled="busy === 'remove'"
                  @click="remove"
                >
                  Remove
                </button>
              </div>
            </template>
          </SessionDetailsPopover>
        </div>
      </div>
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

    <!-- Row 3 — one quiet meta line: repo/branch · agent, then the calm
         conversation-state + freshness pushed to the right. (When attention is
         raised the state lives loudly up on row 1 instead.) -->
    <div class="mt-2 flex items-center gap-2 text-xs">
      <span class="min-w-0 truncate font-mono text-muted">
        {{ repoName(ws.branch.repo_root) }}/{{ ws.branch.name }}
      </span>
      <span class="text-faint">·</span>
      <span class="shrink-0 text-muted">
        {{ ws.agent_kind }}<template v-if="ws.model"> · {{ ws.model }}</template>
      </span>
      <!-- The branch's PR, surfaced inline as a small link — the one place you
           already look — rather than buried in the Overview tab. Compact mode is
           the same tight glyphline the dashboard list uses. -->
      <template v-if="ws.branch.github">
        <span class="text-faint">·</span>
        <GithubStatus :gh="ws.branch.github" compact class="min-w-0" />
      </template>
      <div class="ml-auto flex shrink-0 items-center gap-1.5">
        <span v-if="!loud" data-testid="conversation-state" :class="toneClass">
          {{ conv.glyph }} {{ conv.label }}
        </span>
        <span v-if="!loud && lastActivity" class="text-faint">·</span>
        <span v-if="lastActivity" class="font-mono text-faint">{{ lastActivity }}</span>
      </div>
    </div>

    <!-- Write feedback (rename / acknowledge / archive). Inline so it travels
         with the header on every surface. -->
    <p v-if="error" class="mt-1.5 text-xs text-block">{{ error }}</p>
    <p v-else-if="notice" class="mt-1.5 text-xs text-accent">{{ notice }}</p>
  </header>
</template>
