<script setup lang="ts">
import { computed } from 'vue';
import type { AutomationRun } from '../types';
import { exactTime, timeAgo } from '../lib/time';
import StatusBadge from './StatusBadge.vue';

const props = defineProps<{ run: AutomationRun; intervention: boolean; history?: boolean }>();

const title = computed(() => {
  if (props.intervention) return `Launch ${props.run.status}`;
  if (props.history) return `Run ${props.run.status}`;
  return 'Provisioning session';
});
</script>

<template>
  <li
    class="flex items-start gap-3 border-b border-line px-3 py-2.5 last:border-0"
    data-testid="automation-run-only"
    :data-run-id="run.id"
  >
    <span
      class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
      :class="intervention ? 'bg-block-line' : 'bg-info-line'"
      aria-hidden="true"
    ></span>
    <div class="min-w-0 flex-1">
      <div class="flex flex-wrap items-center gap-2">
        <span class="font-serif text-[15px] font-semibold text-fg">
          {{ title }}
        </span>
        <StatusBadge :status="run.status" />
      </div>
      <p v-if="intervention && run.summary" class="mt-0.5 break-words font-mono text-xs text-block">
        {{ run.summary }}
      </p>
    </div>
    <div class="shrink-0 text-right font-mono text-2xs text-faint">
      <div>{{ run.source }} · {{ run.service_tag }}</div>
      <div>{{ run.profile }}</div>
      <time
        :datetime="run.updated_at"
        :title="exactTime(run.updated_at)"
        :aria-label="exactTime(run.updated_at)"
      >
        {{ timeAgo(run.updated_at) }}
      </time>
    </div>
  </li>
</template>
