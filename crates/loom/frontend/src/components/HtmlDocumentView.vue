<script setup lang="ts">
import { ref, watch } from 'vue';

// An HTML document rendered inside a sandboxed <iframe>.
// The agent hands over a self-contained HTML page (a report, a chart, a tiny
// interactive demo) and we show it as it would look in a browser — not as
// source.
//
// Isolation is the whole point: the frame is `srcdoc` with `allow-scripts` but
// *not* `allow-same-origin`, so the document runs in a unique opaque origin. Its
// scripts execute, but it cannot read loom's cookies, localStorage, or call the
// API as the signed-in user — a hostile artifact is sealed off from the session
// it was written to. (Combining allow-scripts with allow-same-origin would let
// it script its way out of the sandbox, so we never do.)
const props = defineProps<{
  /** Raw HTML source of the artifact (the selected revision's content). */
  content: string;
}>();

// Keep fullscreen inside the sandbox. A blob URL opened in another tab would
// inherit Loom's origin and let document scripts reach the signed-in API.
const container = ref<HTMLElement | null>(null);
function fullscreen() {
  container.value?.requestFullscreen();
}

// Bumping the key remounts the iframe when the content changes (a rev switch or
// an SSE refresh), so a fresh document paints rather than a half-updated DOM.
const reloadKey = ref(0);
watch(
  () => props.content,
  () => (reloadKey.value += 1),
);
</script>

<template>
  <!-- A white backdrop so an unstyled document (no <body> background of its own)
       stays legible regardless of loom's light/dark theme — the artifact owns
       its look from there. -->
  <div ref="container" class="relative h-full w-full bg-white">
    <iframe
      :key="reloadKey"
      :srcdoc="content"
      sandbox="allow-scripts allow-popups allow-popups-to-escape-sandbox allow-forms allow-modals"
      class="h-full w-full border-0 bg-white"
      title="HTML document"
      data-testid="artifact-html"
    ></iframe>
    <button
      class="absolute right-2 top-2 rounded border border-line bg-surface/90 px-2 py-0.5 text-xs text-muted shadow-sm hover:bg-subtle hover:text-fg"
      title="View document fullscreen"
      @click="fullscreen"
    >
      ↗ Fullscreen
    </button>
  </div>
</template>
