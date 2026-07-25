<script setup lang="ts">
import NewSessionDrawer from '../components/NewSessionDrawer.vue';

defineOptions({ name: 'SessionLaunch' });
</script>

<template>
  <div class="flex min-h-full flex-1" data-testid="session-launch-page">
    <NewSessionDrawer />
  </div>
</template>
