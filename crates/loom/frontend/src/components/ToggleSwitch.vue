<script setup lang="ts">
// The one boolean control (docs/loom-ui.md): a compact switch used for
// overlooker enablement and bool settings. The knob carries a hairline ring
// (no shadow — borders, not shadows) so it reads against both track states in
// both palettes. State is exposed via the switch role's aria-checked.
defineProps<{
  modelValue: boolean;
  disabled?: boolean;
  title?: string;
}>();
defineEmits<{ 'update:modelValue': [boolean] }>();
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :disabled="disabled"
    :title="title"
    :class="[
      'relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors disabled:opacity-50',
      modelValue ? 'bg-accent' : 'bg-subtle-hover',
    ]"
    @click="$emit('update:modelValue', !modelValue)"
  >
    <span
      :class="[
        'inline-block h-4 w-4 transform rounded-full bg-surface ring-1 ring-line transition-transform',
        modelValue ? 'translate-x-4' : 'translate-x-0.5',
      ]"
    ></span>
  </button>
</template>
