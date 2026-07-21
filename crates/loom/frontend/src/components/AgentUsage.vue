<script setup lang="ts">
import { computed } from 'vue';
import type { AcpUsage } from '../types';
import { formatTokens } from '../lib/usage';

const props = withDefaults(defineProps<{ usage: AcpUsage; compact?: boolean }>(), {
  compact: false,
});

const percent = computed(() =>
  props.usage.size > 0 ? Math.max(0, (props.usage.used / props.usage.size) * 100) : 0,
);
const barPercent = computed(() => Math.min(100, percent.value));
const tone = computed(() => {
  if (percent.value > 95) return 'critical';
  if (percent.value >= 75) return 'warning';
  return 'normal';
});

const costLabel = computed(() => {
  const cost = props.usage.cost;
  if (!cost || !Number.isFinite(cost.amount)) return '';
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: cost.currency,
      maximumFractionDigits: 4,
    }).format(cost.amount);
  } catch {
    return `${cost.amount.toFixed(4)} ${cost.currency}`;
  }
});
const label = computed(
  () =>
    `${formatTokens(props.usage.used)} / ${formatTokens(props.usage.size)} context · ${Math.round(percent.value)}%`,
);
const title = computed(() => {
  const remaining = Math.max(0, props.usage.size - props.usage.used);
  const pieces = [`${formatTokens(remaining)} tokens remain in the current context window`];
  if (costLabel.value) pieces.push(`${costLabel.value} reported cost`);
  return pieces.join(' · ');
});
</script>

<template>
  <div
    class="agent-usage"
    :class="[`agent-usage--${tone}`, { 'agent-usage--compact': compact }]"
    data-testid="agent-usage"
    :title="title"
  >
    <span class="agent-usage-label">{{ label }}</span>
    <span v-if="!compact" class="agent-usage-track" aria-hidden="true">
      <span class="agent-usage-bar" :style="{ width: `${barPercent}%` }"></span>
    </span>
    <span v-if="!compact && costLabel" class="agent-usage-cost">{{ costLabel }}</span>
  </div>
</template>

<style scoped>
.agent-usage {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 0.5rem;
  color: var(--muted);
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  font-variant-numeric: tabular-nums;
}
.agent-usage--compact {
  color: var(--faint);
  white-space: nowrap;
}
.agent-usage--warning {
  color: var(--attn);
}
.agent-usage--critical {
  color: var(--block);
}
.agent-usage-label {
  white-space: nowrap;
}
.agent-usage-track {
  width: clamp(4rem, 12vw, 8rem);
  height: 0.25rem;
  overflow: hidden;
  border-radius: 999px;
  background: var(--line);
}
.agent-usage-bar {
  display: block;
  height: 100%;
  border-radius: inherit;
  background: var(--accent);
}
.agent-usage--warning .agent-usage-bar {
  background: var(--attn);
}
.agent-usage--critical .agent-usage-bar {
  background: var(--block);
}
.agent-usage-cost {
  color: var(--faint);
  white-space: nowrap;
}
</style>
