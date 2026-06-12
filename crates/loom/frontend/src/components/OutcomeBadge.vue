<script setup lang="ts">
import { computed } from 'vue';

// The outcome of an overlooker round (`ok | noop | skipped | error`), as a
// small, quiet badge. Like the rest of the panel it leans NEUTRAL: `ok` reads
// as a calm "all good", `noop` recedes to faint (nothing to do), and only the
// genuinely-wrong states (`error`, `skipped`) borrow the soft attention/block
// hues — never a loud fill, so the fleet's own attention signal stays the loud
// one. `null` (never run) renders a dim em-dash chip.
const props = defineProps<{ outcome: string | null }>();

interface Style {
  label: string;
  cls: string;
}

const styles: Record<string, Style> = {
  ok: { label: 'ok', cls: 'bg-accent/10 text-accent ring-1 ring-inset ring-accent/30' },
  noop: { label: 'no-op', cls: 'text-faint ring-1 ring-inset ring-line' },
  skipped: { label: 'skipped', cls: 'bg-attn-soft text-attn ring-1 ring-inset ring-attn-line' },
  error: { label: 'error', cls: 'bg-block-soft text-block ring-1 ring-inset ring-block-line' },
};

const style = computed<Style>(() => {
  if (!props.outcome) return { label: 'never run', cls: 'text-faint ring-1 ring-inset ring-line' };
  return styles[props.outcome] ?? { label: props.outcome, cls: 'text-muted ring-1 ring-inset ring-line' };
});
</script>

<template>
  <span
    :class="style.cls"
    data-testid="outcome-badge"
    :data-outcome="outcome ?? 'none'"
    class="inline-flex items-center rounded px-1.5 py-0.5 text-2xs font-medium uppercase tracking-wide font-mono"
  >
    {{ style.label }}
  </span>
</template>
