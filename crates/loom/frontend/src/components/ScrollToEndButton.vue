<script setup lang="ts">
// A small floating affordance that appears at the transcript's lower-right when
// the reader has scrolled away from the newest exchange. Clicking it re-pins the
// view and jumps to the foot. Shared by both conversation surfaces (ACP and
// terminal), which own the scroll container and pass `visible` from their
// follow-the-foot pin state.
defineProps<{ visible: boolean }>();
defineEmits<{ click: [] }>();
</script>

<template>
  <button
    v-if="visible"
    type="button"
    data-testid="scroll-to-end"
    class="scroll-to-end"
    title="Scroll to the latest message"
    aria-label="Scroll to the latest message"
    @click="$emit('click')"
  >
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="m7 6 5 5 5-5" />
      <path d="m7 13 5 5 5-5" />
    </svg>
  </button>
</template>

<style scoped>
.scroll-to-end {
  /* Sticky, not absolute: sitting at the end of the transcript it stays glued to
     the scrollport's lower-right while the reader is up in the history, without
     needing a positioned wrapper around the scroller. */
  position: sticky;
  bottom: 1rem;
  z-index: 10;
  display: grid;
  width: 2rem;
  height: 2rem;
  margin-top: 0.5rem;
  margin-right: 0.5rem;
  margin-left: auto;
  place-items: center;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface);
  color: var(--muted);
  box-shadow: 0 2px 8px rgb(0 0 0 / 0.16);
  cursor: pointer;
  transition:
    color 0.12s ease,
    background-color 0.12s ease,
    border-color 0.12s ease;
}
.scroll-to-end:hover {
  border-color: color-mix(in srgb, var(--accent) 45%, var(--line));
  background: var(--subtle-hover);
  color: var(--fg);
}
.scroll-to-end:focus-visible {
  outline: 2px solid color-mix(in srgb, var(--accent) 55%, transparent);
  outline-offset: 2px;
}
</style>
