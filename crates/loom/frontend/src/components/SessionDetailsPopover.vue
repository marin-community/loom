<script setup lang="ts">
import type { Session } from '../types';

// The "⌄ details" popover for the page header: the low-frequency identity +
// machine metadata trimmed out of the header run (id, branch, base, tmux,
// worktree, github) plus the standalone Files-route affordance. Read-only; the
// page owns open-state.
const props = defineProps<{ ws: Session; open: boolean }>();
const emit = defineEmits<{ 'update:open': [boolean] }>();

function close() {
  emit('update:open', false);
}
</script>

<template>
  <div v-if="open" class="relative">
    <!-- Transparent backdrop dismisses on outside click — dependency-free. -->
    <div class="fixed inset-0 z-10" @click="close"></div>
    <div
      data-testid="details-popover"
      class="absolute right-0 z-20 mt-1 w-80 rounded border border-line bg-surface p-3 shadow-lg"
    >
      <dl class="space-y-2 text-xs">
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
          <dt class="w-16 shrink-0 text-faint">tmux</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.tmux_session }}</dd>
        </div>
        <div class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">worktree</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.work_dir }}</dd>
        </div>
        <div v-if="ws.github_repo" class="flex gap-2">
          <dt class="w-16 shrink-0 text-faint">github</dt>
          <dd class="min-w-0 break-all font-mono text-muted">{{ ws.github_repo }}</dd>
        </div>
      </dl>
      <div class="mt-3 border-t border-line pt-2">
        <router-link
          :to="`/s/${ws.id}/files`"
          class="text-xs text-accent hover:underline"
        >browse files →</router-link>
      </div>
    </div>
  </div>
</template>
