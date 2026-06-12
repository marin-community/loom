<script setup lang="ts">
import { computed } from 'vue';
import type { SignalChip } from '../lib/sessionState';

// A loud attention signal as an individually-dismissable chip: severity fill
// (amber for attention, red for blocked), the agent's plain dot or an
// overlooker's ⊙ "watched" glyph, and a × that clears the underlying tag. This
// replaces the old full-tile wash + "Mark OK" control — every signal is now a
// chip you delete, the agent's `attention` and an overlooker's `triage` each on
// its own. A stale triage mark (the session has moved on since it was set) fades
// but stays clearable, so it can never get "stuck" lit. Quiet free-form tags use
// TagPill; the loud amber/red fill is reserved for these. Tokens auto-swap
// light/dark. `readonly` drops the × for contexts that show but don't edit.
const props = defineProps<{ chip: SignalChip; busy?: boolean; readonly?: boolean }>();
const emit = defineEmits<{ clear: [key: string] }>();

const label = computed(() => (props.chip.level === 'blocked' ? 'Blocked' : 'Needs attention'));
const cls = computed(() =>
  props.chip.level === 'blocked' ? 'bg-block text-block-fg' : 'bg-attn text-attn-fg',
);
const fromOverlooker = computed(() => props.chip.raisedBy === 'triage');

const tooltip = computed(() => {
  if (fromOverlooker.value) {
    const who = props.chip.by && props.chip.by !== 'manual' ? ` (${props.chip.by})` : '';
    const base = props.chip.note
      ? `Overlooker${who}: ${props.chip.note}`
      : `Raised by overlooker${who}`;
    return props.chip.stale ? `${base} — stale, session has moved on` : base;
  }
  return props.chip.note || label.value;
});
</script>

<template>
  <span
    :class="[cls, chip.stale ? 'opacity-60' : '']"
    data-testid="signal-chip"
    :data-signal-key="chip.key"
    :data-level="chip.level"
    :data-raised-by="chip.raisedBy"
    :data-stale="chip.stale ? 'true' : 'false'"
    :title="tooltip"
    class="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-2xs font-medium uppercase tracking-wide"
  >
    <span v-if="fromOverlooker" aria-hidden="true">⊙</span>
    <span v-else class="h-1.5 w-1.5 rounded-full bg-current opacity-80"></span>
    {{ label }}
    <button
      v-if="!readonly"
      type="button"
      data-testid="signal-chip-clear"
      class="-mr-0.5 shrink-0 rounded px-0.5 opacity-70 hover:opacity-100 disabled:opacity-40"
      :disabled="busy"
      :title="`Clear ${chip.key}`"
      @click.stop="emit('clear', chip.key)"
    >
      ×
    </button>
  </span>
</template>
