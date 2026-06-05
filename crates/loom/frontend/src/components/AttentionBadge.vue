<script setup lang="ts">
import { computed } from 'vue';

// The agent-declared attention axis: does this session need me? This is the ONE
// reserved loud signal in the UI. `ok` is intentionally a quiet ghost chip — it
// should recede so the loud amber/red states are the only thing that pops.
// Colors come from semantic tokens (attn/block) so they auto-swap light/dark.
const props = defineProps<{ level: string; note?: string }>();

interface Style {
  label: string;
  cls: string;
  dot: string;
}

const styles: Record<string, Style> = {
  ok: {
    label: 'OK',
    cls: 'bg-transparent text-faint ring-1 ring-inset ring-line',
    dot: 'bg-faint',
  },
  attention: {
    label: 'Attention',
    cls: 'bg-attn text-attn-fg',
    dot: 'bg-attn-fg/80',
  },
  blocked: {
    label: 'Blocked',
    cls: 'bg-block text-block-fg',
    dot: 'bg-block-fg/80',
  },
};

const style = computed(
  () => styles[props.level] ?? { label: props.level, cls: 'bg-transparent text-faint ring-1 ring-inset ring-line', dot: 'bg-faint' },
);
</script>

<template>
  <span
    :class="style.cls"
    data-testid="attention-badge"
    :data-level="level"
    :title="note || style.label"
    class="inline-flex items-center gap-1.5 rounded px-2 py-0.5 text-xs font-medium uppercase tracking-wide"
  >
    <span :class="style.dot" class="h-1.5 w-1.5 rounded-full"></span>
    {{ style.label }}
  </span>
</template>
