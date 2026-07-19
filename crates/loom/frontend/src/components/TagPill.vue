<script setup lang="ts">
import { computed } from 'vue';
import type { Tag } from '../types';

// A quiet, free-form tag rendered as a deletable pill: `key:value` (or just the
// value when it carries the meaning), with a × that clears it. Quiet styling
// only — never the reserved loud amber/red fill, which belongs to the single
// resolved attention signal. Tokens (subtle/muted/faint) auto-swap light/dark.
// `readonly` drops the × so the same pill renders in contexts where tags are
// shown but not edited (e.g. a session's read-only issue list).
const props = defineProps<{ tag: Tag; busy?: boolean; readonly?: boolean }>();
const emit = defineEmits<{ clear: [key: string] }>();

const label = computed(() =>
  props.tag.value ? `${props.tag.key}: ${props.tag.value}` : props.tag.key,
);

const tooltip = computed(() => {
  const who = props.tag.set_by && props.tag.set_by !== 'manual' ? ` · ${props.tag.set_by}` : '';
  return props.tag.note ? `${props.tag.note}${who}` : `${props.tag.key}${who}`;
});
</script>

<template>
  <span class="tag-pill" data-testid="tag-pill" :data-tag-key="tag.key" :title="tooltip">
    <span class="truncate">{{ label }}</span>
    <button
      v-if="!readonly"
      type="button"
      data-testid="tag-pill-clear"
      class="-mr-1 shrink-0 rounded px-1 text-faint hover:text-fg disabled:opacity-50"
      :disabled="busy"
      :title="`Clear ${tag.key}`"
      @click.stop="emit('clear', tag.key)"
    >
      ×
    </button>
  </span>
</template>
