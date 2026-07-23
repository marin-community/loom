<script setup lang="ts">
import { computed } from 'vue';
import type { AutomationRun, Session } from '../types';
import {
  byAutomationPriority,
  isAutomationHistory,
  needsAutomationIntervention,
  runNeedsIntervention,
  unmatchedAutomationRuns,
} from '../lib/automationSessions';
import AutomationRunRow from './AutomationRunRow.vue';
import AutomationSessionRow from './AutomationSessionRow.vue';

const props = defineProps<{
  sessions: Session[];
  fleet: Session[];
  runs: AutomationRun[];
  historyOpen: boolean;
  clearingTag: string;
}>();

const emit = defineEmits<{
  toggleHistory: [];
  clearTag: [sessionId: string, key: string];
}>();

const interventions = computed(() =>
  props.sessions.filter(needsAutomationIntervention).sort(byAutomationPriority),
);
const active = computed(() =>
  props.sessions
    .filter((session) => !isAutomationHistory(session) && !needsAutomationIntervention(session))
    .sort(byAutomationPriority),
);
const history = computed(() =>
  props.sessions
    .filter(isAutomationHistory)
    .sort((a, b) => (b.last_activity_at || '').localeCompare(a.last_activity_at || '')),
);
const unmatched = computed(() => unmatchedAutomationRuns(props.runs, props.sessions));
const failedRuns = computed(() =>
  unmatched.value
    .filter(runNeedsIntervention)
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)),
);
const creatingRuns = computed(() =>
  unmatched.value
    .filter((run) => !runNeedsIntervention(run))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)),
);

const parentById = computed(() => {
  const index = new Map<string, Session>();
  for (const session of props.fleet) index.set(session.branch.id, session);
  return index;
});
</script>

<template>
  <div class="space-y-4" data-testid="automation-pane">
    <section aria-labelledby="automation-intervention-heading">
      <div class="mb-1.5 flex items-center gap-2">
        <h2
          id="automation-intervention-heading"
          class="text-2xs font-semibold uppercase tracking-wider text-muted"
        >
          Needs intervention
        </h2>
        <span
          class="rounded-full bg-block-soft px-1.5 font-mono text-2xs text-block"
          :aria-label="`${interventions.length + failedRuns.length} automation runs need intervention`"
        >
          {{ interventions.length + failedRuns.length }}
        </span>
      </div>

      <ul
        v-if="interventions.length || failedRuns.length"
        class="rounded-md border border-line bg-surface"
        data-testid="automation-interventions"
      >
        <AutomationSessionRow
          v-for="session in interventions"
          :key="session.id"
          :session="session"
          :parent="session.parent_id ? parentById.get(session.parent_id) : undefined"
          tone="intervention"
          :clearing-tag="clearingTag"
          @clear-tag="(key) => emit('clearTag', session.id, key)"
        />

        <AutomationRunRow v-for="run in failedRuns" :key="run.id" :run="run" intervention />
      </ul>
      <p v-else class="rounded-md border border-dashed border-line px-3 py-3 text-sm text-muted">
        No automation needs intervention.
      </p>
    </section>

    <section aria-labelledby="automation-active-heading">
      <div class="mb-1.5 flex items-center gap-2">
        <h2
          id="automation-active-heading"
          class="text-2xs font-semibold uppercase tracking-wider text-muted"
        >
          Active
        </h2>
        <span class="rounded-full bg-subtle px-1.5 font-mono text-2xs text-faint">
          {{ active.length + creatingRuns.length }}
        </span>
      </div>

      <ul
        v-if="active.length || creatingRuns.length"
        class="rounded-md border border-line bg-surface"
        data-testid="automation-active"
      >
        <AutomationSessionRow
          v-for="session in active"
          :key="session.id"
          :session="session"
          :parent="session.parent_id ? parentById.get(session.parent_id) : undefined"
          tone="active"
          :clearing-tag="clearingTag"
          @clear-tag="(key) => emit('clearTag', session.id, key)"
        />

        <AutomationRunRow
          v-for="run in creatingRuns"
          :key="run.id"
          :run="run"
          :intervention="false"
        />
      </ul>
      <p v-else class="rounded-md border border-dashed border-line px-3 py-3 text-sm text-muted">
        No automation is active.
      </p>
    </section>

    <section aria-labelledby="automation-history-heading">
      <button
        type="button"
        class="flex w-full items-center gap-2 py-1.5 text-left text-2xs font-medium uppercase tracking-wider text-faint hover:text-muted"
        :aria-expanded="historyOpen"
        aria-controls="automation-history-list"
        data-testid="automation-history-toggle"
        @click="emit('toggleHistory')"
      >
        <span class="inline-block w-2 transition-transform" :class="historyOpen ? 'rotate-90' : ''"
          >▸</span
        >
        <span id="automation-history-heading">History</span>
        <span class="font-mono lowercase tracking-normal">{{ history.length }}</span>
        <span class="h-px flex-1 bg-line"></span>
      </button>

      <ul
        v-show="historyOpen"
        id="automation-history-list"
        class="rounded-md border border-line bg-surface"
        data-testid="automation-history"
      >
        <AutomationSessionRow
          v-for="session in history"
          :key="session.id"
          :session="session"
          :parent="session.parent_id ? parentById.get(session.parent_id) : undefined"
          tone="history"
          :clearing-tag="clearingTag"
        />
        <li v-if="!history.length" class="px-3 py-4 text-center text-sm text-muted">
          No automation history yet.
        </li>
      </ul>
    </section>
  </div>
</template>
