<script setup lang="ts">
import { theme, toggleTheme } from './theme';
</script>

<template>
  <div class="min-h-screen bg-canvas text-fg font-sans">
    <header class="border-b border-line px-6 py-3 flex items-center gap-3">
      <router-link to="/" class="text-lg font-semibold tracking-tight">loom</router-link>
      <span class="text-muted text-sm">agent sessions</span>
      <button
        type="button"
        class="ml-auto rounded p-1.5 text-muted hover:text-fg hover:bg-subtle"
        :title="theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'"
        aria-label="Toggle color theme"
        @click="toggleTheme"
      >
        <span v-if="theme === 'dark'">☀</span>
        <span v-else>☾</span>
      </button>
      <router-link
        to="/overlookers"
        class="text-sm text-muted hover:text-fg"
      >
        Overlookers
      </router-link>
      <router-link
        to="/settings"
        class="text-sm text-muted hover:text-fg"
      >
        Settings
      </router-link>
    </header>
    <main class="px-6 py-5 max-w-6xl mx-auto">
      <router-view />
    </main>
  </div>
</template>
