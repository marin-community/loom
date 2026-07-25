<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from 'vue';
import type { Session } from '../types';

// The Details popover for the page header. Holds everything low-frequency so it
// stays out of the always-visible header run, yet reachable from any scroll
// position. Two stacked sections:
//   • lifecycle actions, injected by the header via the #actions slot
//   • identity / machine metadata (id, branch, base, terminal, worktree, github)
//
// Actions come first, under a heading: they are the reason a human opens this,
// and burying them under a scrolling metadata list is what made adopt/archive so
// hard to find. The metadata is reference material — it can take second place
// and scroll.
const props = defineProps<{ ws: Session; open: boolean }>();
const emit = defineEmits<{ 'update:open': [boolean] }>();
const panel = ref<HTMLElement | null>(null);

function close() {
  emit('update:open', false);
}

// This is a nonmodal popover: a pointer press elsewhere light-dismisses it but
// is not swallowed, so the outside control still receives the same press. The
// trigger is exempt because its own click toggles the state.
function onOutsidePointerDown(event: PointerEvent) {
  const target = event.target;
  if (!(target instanceof Element) || panel.value?.contains(target)) return;
  if (target.closest('[aria-controls="session-details-popover"]')) return;
  close();
}

watch(
  () => props.open,
  async (open) => {
    document.removeEventListener('pointerdown', onOutsidePointerDown, true);
    if (!open) return;
    await nextTick();
    if (!props.open) return;
    document.addEventListener('pointerdown', onOutsidePointerDown, true);
    const first = panel.value?.querySelector<HTMLElement>(
      'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled)',
    );
    (first ?? panel.value)?.focus();
  },
);
onBeforeUnmount(() => document.removeEventListener('pointerdown', onOutsidePointerDown, true));
</script>

<template>
  <div v-if="open" class="relative">
    <!-- Height is capped to the viewport (the button sits ~3rem from the top;
         ~7rem also clears the status bar with slack) with the metadata
         scrolling internally, so the lifecycle actions below it stay reachable
         in a short window instead of falling past the bottom of the page. -->
    <div
      id="session-details-popover"
      ref="panel"
      data-testid="details-popover"
      role="region"
      aria-label="Session details and actions"
      tabindex="-1"
      class="absolute right-0 z-20 mt-1 flex max-h-[calc(100vh-7rem)] w-[min(20rem,calc(100vw-2rem))] flex-col rounded border border-line bg-surface p-3 shadow-lg outline-none"
      @keydown.esc.stop.prevent="close"
    >
      <!-- Lifecycle actions (Adopt / Recover / Archive / Remove), supplied by the
           header — first, because they are what this menu is for. -->
      <div class="shrink-0">
        <h3 class="mb-1 px-2 text-2xs font-semibold uppercase tracking-wider text-muted">
          Actions
        </h3>
        <slot name="actions" />
      </div>

      <div v-if="$slots.context" class="mt-3 shrink-0 border-t border-line px-2 pt-3">
        <slot name="context" />
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
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">runtime</dt>
          <dd class="min-w-0 break-all text-muted">
            {{ ws.agent_kind }}<template v-if="ws.model"> · {{ ws.model }}</template
            ><template v-if="ws.effort"> · {{ ws.effort }}</template>
          </dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">profile</dt>
          <dd class="min-w-0 break-all font-mono text-muted">
            {{ ws.profile }} · v{{ ws.profile_revision }}
          </dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">policy</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.launch_mode || 'default' }}</dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">source</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.origin }} · {{ ws.class }}</dd>
        </div>
      </dl>
    </div>
  </div>
</template>
