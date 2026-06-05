<script setup lang="ts">
import { computed } from 'vue';
import type { Session } from '../types';
import { conversationState, levelOf, TONE_TEXT } from '../lib/sessionState';
import { timeAgo } from '../lib/time';

// The conversation-state strip: a derived STATE line (glyph + label), the one
// human write on this page (Mark OK — acknowledge the agent's attention), and a
// freshness stamp. Replaces the old verbatim "Waiting for input" prompt slab —
// the prompt itself is live in the terminal one tab away.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ acknowledge: [] }>();

// Mark OK only makes sense when the AGENT has actually raised its attention
// level — acknowledging clears it back to 'ok'.
const ackable = computed(() => levelOf(props.ws) !== 'ok');

// The state line is derived from the agent's DECLARED attention level (set by
// Claude's working/waiting/idle hooks and `weaver set-status`) plus lifecycle —
// deliberately NOT from `pending_prompt`. That captured-pane snapshot is only
// cleared by a `working` hook (an actual user submission), so it lingers on
// watcher/loop sessions and after an acknowledge — it is not a reliable
// "needs input now" signal, and treating it as one shows a phantom block.
const conv = computed(() => conversationState(props.ws));
const toneClass = computed(() => TONE_TEXT[conv.value.tone]);
const lastActivity = computed(() => timeAgo(props.ws.last_activity_at));
// Only true attention/blocked rows get the row wash + slow breath.
const loud = computed(() => conv.value.tone === 'block' || conv.value.tone === 'attn');
const washClass = computed(() =>
  conv.value.tone === 'block'
    ? 'border-l-2 border-block-line bg-block-soft'
    : conv.value.tone === 'attn'
      ? 'border-l-2 border-attn-line bg-attn-soft'
      : 'border-l-2 border-transparent',
);
</script>

<template>
  <div
    class="mb-4 flex items-center gap-2 rounded px-3 py-2 text-sm"
    :class="[washClass, loud ? 'pulse-attention' : '']"
  >
    <span :class="toneClass">{{ conv.glyph }}</span>
    <span :class="toneClass" class="font-medium" data-testid="conversation-state">{{ conv.label }}</span>
    <div class="ml-auto flex items-center gap-3">
      <button
        v-if="ackable"
        type="button"
        data-testid="acknowledge"
        class="rounded bg-surface px-2.5 py-1 text-xs font-semibold text-fg shadow-sm ring-1 ring-inset ring-line hover:bg-subtle"
        @click="emit('acknowledge')"
      >
        Mark OK ✓
      </button>
      <span v-if="lastActivity" class="font-mono text-xs text-faint">
        last activity {{ lastActivity }}
      </span>
    </div>
  </div>
</template>
