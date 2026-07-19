<script setup lang="ts">
import { computed } from 'vue';

// Work-area sub-nav. Every tab is a local flip the parent (SessionDetail) acts
// on: the panes v-show their kept-alive selves; Artifacts drives the route (and
// the lazy artifacts panel) and can be popped out into a rail. Neutral underline
// indicator — no loud fills; only the active tab gets text-fg + an accent
// underline.
//
// The set depends on the execution backend. A *terminal* session leads with
// Terminal (the live agent's TUI): Terminal · Overview · Conversation ·
// Artifacts. An *ACP* session has no agent TUI — its Conversation is the working
// surface, so it leads: Conversation · Overview · Shells · Artifacts, where
// Shells is the worktree escape hatch (the old Terminal tab's reason to be
// first-class — the agent lived there — is gone).
type Tab = 'terminal' | 'overview' | 'conversation' | 'artifacts' | 'shells';

const props = defineProps<{
  tab: Tab;
  id: string;
  issueCount: number;
  /** Artifacts is open in the rail (popped out) rather than the work area. */
  artifactsPopped?: boolean;
  /** Execution backend — selects the tab set + order. */
  protocol?: 'terminal' | 'acp';
}>();
defineEmits<{ select: [Tab] }>();

const TERMINAL_TABS: { key: Tab; label: string }[] = [
  { key: 'terminal', label: 'Terminal' },
  { key: 'overview', label: 'Overview' },
  { key: 'conversation', label: 'Conversation' },
  { key: 'artifacts', label: 'Artifacts' },
];
const ACP_TABS: { key: Tab; label: string }[] = [
  { key: 'conversation', label: 'Conversation' },
  { key: 'overview', label: 'Overview' },
  { key: 'shells', label: 'Shells' },
  { key: 'artifacts', label: 'Artifacts' },
];
const tabs = computed(() => (props.protocol === 'acp' ? ACP_TABS : TERMINAL_TABS));
</script>

<template>
  <!-- pl-0.5 mirrors the header's 2px left wash border so tab labels align
       with the title above. -->
  <nav class="mb-1.5 flex items-center gap-0.5 border-b border-line pl-0.5 text-xs">
    <button
      v-for="t in tabs"
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
      <span
        v-if="t.key === 'artifacts' && artifactsPopped"
        class="ml-1 text-faint"
        title="Open in the side panel"
        >⤢</span
      >
    </button>
    <!-- The tab row's right side is otherwise dead space — hosts compact,
         always-relevant extras (the scratch attach strip on the detail page). -->
    <div class="ml-auto flex min-w-0 items-center">
      <slot name="right" />
    </div>
  </nav>
</template>
