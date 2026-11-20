<script setup lang="ts">
import { computed, ref, watch } from 'vue';

// An inline markdown image that degrades to a card naming the source when
// the load fails — the browser's broken-image icon explains nothing.
const props = defineProps<{
  /** Resolved, fetchable URL; empty when the token carried no src. */
  src: string;
  /** The original markdown source, shown when the load fails. */
  raw: string;
  alt?: string;
}>();

const failed = ref(false);
// Remounts the <img> on retry so the browser re-requests the URL —
// agents often announce a shot before writing it, so the first load can fail
// while the bytes are still on their way. The attempt counter also busts any
// heuristically cached failure, so a retry always asks the server again.
const attempt = ref(0);

watch(
  () => props.src,
  () => {
    failed.value = false;
    attempt.value = 0;
  },
);

const fetchSrc = computed(() => {
  // A `data:` URI has no query string to bust and appending one would
  // corrupt it; its retry just remounts the unchanged source.
  if (!attempt.value || !props.src || props.src.startsWith('data:')) return props.src;
  return `${props.src}${props.src.includes('?') ? '&' : '?'}v=${attempt.value}`;
});

function retry() {
  failed.value = false;
  attempt.value++;
}
</script>

<template>
  <img
    v-if="src && !failed"
    :key="attempt"
    data-testid="markdown-image"
    :src="fetchSrc"
    :alt="alt ?? ''"
    @error="failed = true"
  />
  <span v-else data-testid="markdown-image-fallback" class="markdown-image-fallback" :title="raw">
    <span aria-hidden="true">◧</span>
    <span>{{ alt || 'image' }} unavailable</span>
    <code v-if="raw">{{ raw }}</code>
    <button
      v-if="src"
      type="button"
      class="markdown-image-retry"
      data-testid="markdown-image-retry"
      @click="retry"
    >
      retry
    </button>
  </span>
</template>
