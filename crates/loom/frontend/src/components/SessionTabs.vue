<script setup lang="ts">
import { computed } from 'vue';

// Work-area sub-nav. Every tab is a local flip the parent (SessionDetail) acts
// on: the panes v-show their kept-alive selves; Artifacts drives the route (and
// the lazy artifacts panel) and can be popped out into a rail. Neutral underline
// indicator — no loud fills; only the active tab gets text-fg + an accent
// underline.
//
// The set depends on the execution backend. A terminal session leads with its
// live Agent surface; an ACP session leads with Conversation. Artifacts remains
// route-backed and deep-linkable. Overview was a duplicate of these operational
// surfaces and is deliberately absent.
type Tab = 'terminal' | 'conversation' | 'review' | 'shells';

const props = defineProps<{
  tab: Tab;
  /** Artifacts is open in the rail (popped out) rather than the work area. */
  artifactsPopped?: boolean;
  /** Execution backend — selects the tab set + order. */
  protocol?: 'terminal' | 'acp';
}>();
defineEmits<{ select: [Tab] }>();

const TERMINAL_TABS: { key: Tab; label: string }[] = [
  { key: 'terminal', label: 'Agent' },
  { key: 'conversation', label: 'Conversation' },
  { key: 'review', label: 'Review' },
];
const ACP_TABS: { key: Tab; label: string }[] = [
  { key: 'conversation', label: 'Conversation' },
  { key: 'shells', label: 'Shells' },
  { key: 'review', label: 'Review' },
];
const tabs = computed(() => (props.protocol === 'acp' ? ACP_TABS : TERMINAL_TABS));
</script>

<template>
  <!-- pl-0.5 mirrors the header's 2px left wash border so tab labels align
       with the title above. -->
  <nav
    class="mb-1.5 flex items-center gap-0.5 border-b border-line pl-0.5 text-xs"
    aria-label="Session surfaces"
  >
    <button
      v-for="t in tabs"
      :key="t.key"
      type="button"
      role="tab"
      :data-tab="t.key"
      :aria-selected="tab === t.key"
      class="-mb-px border-b-2 px-2 py-1"
      :class="
        tab === t.key || (t.key === 'review' && artifactsPopped)
          ? 'border-accent text-fg font-medium'
          : 'border-transparent text-muted hover:text-fg'
      "
      @click="$emit('select', t.key)"
    >
      {{ t.label }}
      <!-- When popped out, the Artifacts surface lives in the rail, not here —
           a small glyph marks it open without claiming the work area. -->
      <span
        v-if="t.key === 'review' && artifactsPopped"
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
