<script setup lang="ts">
import AppRail from './components/AppRail.vue';
import StatusBar from './components/StatusBar.vue';

// The workbench shell (docs/loom-ui.md): nav rail on the left, a thin status
// bar pinned to the bottom, and the view filling everything between — no top
// app bar. `main` is a flex column so a full-height view (session detail,
// file browser) can `flex-1 min-h-0` to fill it exactly, while list views
// simply grow and let `main` scroll.
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-canvas font-sans text-fg">
    <AppRail />
    <div class="flex min-w-0 flex-1 flex-col">
      <main class="flex min-h-0 flex-1 flex-col overflow-auto">
        <router-view />
      </main>
      <StatusBar />
    </div>
  </div>
</template>
