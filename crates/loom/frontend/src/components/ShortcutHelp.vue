<script setup lang="ts">
import { nextTick, ref, watch } from 'vue';
import { useCommandRegistry } from '../lib/commands';
import KeyHint from './KeyHint.vue';

const { activeScopes, helpOpen } = useCommandRegistry();
const closeButton = ref<HTMLButtonElement>();
let returnFocus: HTMLElement | null = null;

watch(helpOpen, async (open) => {
  if (open) {
    returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    await nextTick();
    closeButton.value?.focus();
  } else {
    returnFocus?.focus();
    returnFocus = null;
  }
});

function keepFocusInHelp(event: KeyboardEvent) {
  if (event.key !== 'Tab') return;
  event.preventDefault();
  closeButton.value?.focus();
}
</script>

<template>
  <div
    v-if="helpOpen"
    data-testid="shortcut-help"
    class="fixed inset-0 z-[70] flex items-center justify-center bg-black/45 p-4"
    @mousedown.self="helpOpen = false"
  >
    <section
      role="dialog"
      aria-modal="true"
      aria-labelledby="shortcut-help-title"
      class="max-h-[80vh] w-full max-w-xl overflow-auto border border-line bg-surface shadow-xl"
      @keydown="keepFocusInHelp"
    >
      <header class="flex items-center border-b border-line bg-rail px-3 py-2">
        <div>
          <p class="font-mono text-2xs uppercase tracking-wider text-faint">command reference</p>
          <h2 id="shortcut-help-title" class="font-mono text-sm font-semibold text-fg">
            Loom keyboard shortcuts
          </h2>
        </div>
        <button
          ref="closeButton"
          type="button"
          class="btn-secondary ml-auto px-2 py-1 text-xs"
          @click="helpOpen = false"
        >
          <KeyHint keys="Esc" />
          Close
        </button>
      </header>

      <div class="divide-y divide-line">
        <section v-for="scope in activeScopes" :key="scope.id" class="px-3 py-3">
          <h3 class="mb-1.5 font-mono text-2xs font-semibold uppercase tracking-wider text-muted">
            {{ scope.label }}
          </h3>
          <dl class="grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-1.5 text-xs">
            <template v-for="command in scope.commands" :key="command.id">
              <dt :data-command-id="command.id" class="flex flex-wrap gap-1">
                <KeyHint v-for="keys in command.keys" :key="keys" :keys="keys" />
              </dt>
              <dd class="text-fg">{{ command.label }}</dd>
            </template>
          </dl>
        </section>
      </div>
    </section>
  </div>
</template>
