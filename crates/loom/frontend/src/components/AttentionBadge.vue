<script setup lang="ts">
import { computed } from 'vue';

// The agent-declared attention axis: does this session need me? Green = ok,
// amber = wants attention, red = blocked / needs help. Distinct from the
// mechanical lifecycle StatusBadge. Solid status hues (no semantic token
// exists for green/amber/red) read on both light and dark themes.
const props = defineProps<{ level: string; note?: string }>();

interface Style {
  label: string;
  cls: string;
  dot: string;
}

const styles: Record<string, Style> = {
  ok: { label: 'OK', cls: 'bg-emerald-700 text-emerald-50', dot: 'bg-emerald-300' },
  attention: { label: 'Attention', cls: 'bg-amber-600 text-amber-50', dot: 'bg-amber-200' },
  blocked: { label: 'Blocked', cls: 'bg-red-700 text-red-50', dot: 'bg-red-300' },
};

const style = computed(
  () => styles[props.level] ?? { label: props.level, cls: 'bg-subtle text-fg', dot: 'bg-faint' },
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
