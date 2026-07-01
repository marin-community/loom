<script setup lang="ts">
// Work-area sub-nav. Every tab is a local flip the parent (SessionDetail) acts
// on: Terminal/Overview/Conversation v-show their kept-alive panes; Artifacts
// drives the route (and the lazy artifacts panel) and can be popped out into a
// rail beside the terminal. Neutral underline indicator — no loud fills; only
// the active tab gets text-fg + an accent underline.
//
// Terminal is the working zone (the live agent); Overview is the read-only
// context (goal, claimed issues, activity) — the issue count rides on the
// Overview tab as a quiet pill. Artifacts is the agent's out-of-repo documents
// (designs, reports, the plan). The worktree files live in the embedded editor
// (the side panel), not a tab.
type Tab = 'terminal' | 'overview' | 'conversation' | 'artifacts';

defineProps<{
  tab: Tab;
  id: string;
  issueCount: number;
  /** Artifacts is open in the rail (popped out) rather than the work area. */
  artifactsPopped?: boolean;
}>();
defineEmits<{ select: [Tab] }>();

const TABS: { key: Tab; label: string }[] = [
  { key: 'terminal', label: 'Terminal' },
  { key: 'overview', label: 'Overview' },
  // The agent's chat with the model — live, and (via the archive capture) still
  // here to review after the terminal is gone.
  { key: 'conversation', label: 'Conversation' },
  // The agent's out-of-repo documents; lazily mounts its (heavy) viewer.
  { key: 'artifacts', label: 'Artifacts' },
];
</script>

<template>
  <!-- pl-0.5 mirrors the header's 2px left wash border so tab labels align
       with the title above. -->
  <nav class="mb-1.5 flex items-center gap-0.5 border-b border-line pl-0.5 text-xs">
    <button
      v-for="t in TABS"
      :key="t.key"
      type="button"
      :data-tab="t.key"
      class="-mb-px border-b-2 px-2 py-1"
      :class="
        tab === t.key || (t.key === 'artifacts' && artifactsPopped)
          ? 'border-accent text-fg font-medium'
          : 'border-transparent text-muted hover:text-fg'
      "
      @click="$emit('select', t.key)"
    >
      {{ t.label }}
      <span v-if="t.key === 'overview' && issueCount" class="pill ml-1">{{ issueCount }}</span>
      <!-- When popped out, the Artifacts surface lives in the rail, not here —
           a small glyph marks it open without claiming the work area. -->
      <span v-if="t.key === 'artifacts' && artifactsPopped" class="ml-1 text-faint" title="Open in the side panel">⤢</span>
    </button>
    <!-- The tab row's right side is otherwise dead space — hosts compact,
         always-relevant extras (the scratch attach strip on the detail page). -->
    <div class="ml-auto flex min-w-0 items-center">
      <slot name="right" />
    </div>
  </nav>
</template>
