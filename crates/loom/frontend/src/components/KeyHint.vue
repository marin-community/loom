<script setup lang="ts">
defineProps<{ keys: string }>();
</script>

<template>
  <kbd
    class="inline-flex min-w-4 items-center justify-center border border-line bg-input px-1 font-mono text-[10px] font-medium leading-4 text-muted"
  >
    {{ keys }}
  </kbd>
</template>
