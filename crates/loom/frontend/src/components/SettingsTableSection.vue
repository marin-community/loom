<script setup lang="ts">
defineProps<{
  columns: string[];
  gridClass: string;
}>();
</script>

<template>
  <section class="overflow-hidden rounded-md border border-line bg-surface">
    <div
      class="grid grid-cols-1 gap-2 border-b border-line bg-surface px-3 py-2 text-2xs uppercase tracking-wider text-muted"
      :class="gridClass"
    >
      <span
        v-for="(column, index) in columns"
        :key="column"
        :class="index > 0 ? 'hidden md:block' : ''"
      >
        {{ column }}
      </span>
    </div>
    <slot />
  </section>
</template>
