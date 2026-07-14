<script setup lang="ts">
import type { Session } from '../types';

// The "⋯ manage" menu for the page header. Holds everything low-frequency so it
// stays out of the always-visible header run, yet reachable from any scroll
// position (the lifecycle actions used to sit at the bottom of a long Overview,
// where they scrolled out of sight). Two stacked sections:
//   • lifecycle actions, injected by the header via the #actions slot
//   • identity / machine metadata (id, branch, base, terminal, worktree, github)
//
// Actions come first, under a heading: they are the reason a human opens this,
// and burying them under a scrolling metadata list is what made adopt/archive so
// hard to find. The metadata is reference material — it can take second place
// and scroll.
defineProps<{ ws: Session; open: boolean }>();
const emit = defineEmits<{ 'update:open': [boolean] }>();

function close() {
  emit('update:open', false);
}
</script>

<template>
  <div v-if="open" class="relative">
    <!-- Transparent backdrop dismisses on outside click — dependency-free. -->
    <div class="fixed inset-0 z-10" @click="close"></div>
    <!-- Height is capped to the viewport (the button sits ~3rem from the top;
         ~7rem also clears the status bar with slack) with the metadata
         scrolling internally, so the lifecycle actions below it stay reachable
         in a short window instead of falling past the bottom of the page. -->
    <div
      data-testid="details-popover"
      class="absolute right-0 z-20 mt-1 flex max-h-[calc(100vh-7rem)] w-80 flex-col rounded border border-line bg-surface p-3 shadow-lg"
    >
      <!-- Lifecycle actions (Adopt / Recover / Archive / Remove), supplied by the
           header — first, because they are what this menu is for. -->
      <div class="shrink-0">
        <h3 class="mb-1 px-2 text-2xs font-semibold uppercase tracking-wider text-muted">
          Actions
        </h3>
        <slot name="actions" />
      </div>

      <h3
        class="mb-1 mt-3 shrink-0 border-t border-line px-2 pt-3 text-2xs font-semibold uppercase tracking-wider text-muted"
      >
        Details
      </h3>
      <dl class="min-h-0 space-y-2 overflow-y-auto px-2 text-xs">
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">id</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.id }}</dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">branch</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.branch.branch }}</dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">base</dt>
          <dd class="min-w-0 break-all font-mono text-muted">base {{ ws.branch.base_branch }}</dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">terminal</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.term_session }}</dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">worktree</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.work_dir }}</dd>
        </div>
        <div v-if="ws.github_repo" class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">github</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.github_repo }}</dd>
        </div>
        <div v-if="ws.created_by" class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">created by</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.created_by }}</dd>
        </div>
      </dl>
    </div>
  </div>
</template>
