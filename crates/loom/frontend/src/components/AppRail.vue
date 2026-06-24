<script setup lang="ts">
import { computed } from 'vue';
import { useRoute } from 'vue-router';
import { theme, toggleTheme } from '../theme';

// The workbench nav rail — the app's only chrome besides the status bar
// (see docs/loom-ui.md). Icon+label items down the left edge; the active view
// carries a 2px accent bar on its left (the VS Code activity-bar idiom).
// Settings and the theme toggle pin to the bottom.
const route = useRoute();

interface RailItem {
  to: string;
  /** The short rail caption (the rail is 56px — long names don't fit). */
  label: string;
  /** Tooltip; defaults to the label. Lets "Watch" expand to "Overlookers". */
  title?: string;
  /** Active when the current path matches one of these prefixes ('/' is exact,
   *  plus the session pages which drill down from the fleet list). */
  match: (path: string) => boolean;
  /** Inline SVG path data (lucide outlines, 24px grid, stroked). */
  paths: string[];
}

const MAIN: RailItem[] = [
  {
    to: '/',
    label: 'Sessions',
    match: (p) => p === '/' || p.startsWith('/s/'),
    // square-terminal — a session is a live agent terminal.
    paths: ['m7 11 2-2-2-2', 'M13 15h4', 'M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Z'],
  },
  {
    to: '/issues',
    label: 'Issues',
    match: (p) => p.startsWith('/issues'),
    // circle-dot — the issue-tracker glyph.
    paths: ['M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Z', 'M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2Z'],
  },
  {
    to: '/chat',
    label: 'Chat',
    title: 'Chat — ask the concierge about your fleet and steer it',
    match: (p) => p.startsWith('/chat'),
    // messages-square — a conversation about the looms.
    paths: [
      'M14 9a2 2 0 0 1-2 2H6l-4 4V4a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2Z',
      'M18 9h2a2 2 0 0 1 2 2v11l-4-4h-6a2 2 0 0 1-2-2v-1',
    ],
  },
  {
    to: '/overlookers',
    label: 'Watch',
    title: 'Overlookers — watch agents over the fleet',
    match: (p) => p.startsWith('/overlookers'),
    // eye — the watchers over the fleet.
    paths: [
      'M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0',
      'M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z',
    ],
  },
  {
    to: '/shell',
    label: 'Shell',
    title: 'Scratch shell — a login shell in the container (e.g. gcloud auth login)',
    match: (p) => p.startsWith('/shell'),
    // terminal — a bare prompt for operator setup.
    paths: ['m4 17 6-6-6-6', 'M12 19h8'],
  },
];

const SETTINGS: RailItem = {
  to: '/settings',
  label: 'Settings',
  match: (p) => p.startsWith('/settings'),
  paths: [
    'M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z',
    'M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z',
  ],
};

const active = computed(() => (item: RailItem) => item.match(route.path));
</script>

<template>
  <nav
    class="flex w-14 shrink-0 flex-col items-stretch border-r border-line bg-rail"
    aria-label="Primary"
  >
    <!-- Wordmark — a warp/weft weave glyph; home link to the fleet. -->
    <router-link
      to="/"
      class="flex h-12 items-center justify-center text-accent"
      title="loom — agent sessions"
      aria-label="loom home"
    >
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.75" stroke-linecap="round" aria-hidden="true">
        <path d="M4 9h16M4 15h16M9 4v16M15 4v16" />
      </svg>
    </router-link>

    <router-link
      v-for="item in MAIN"
      :key="item.to"
      :to="item.to"
      :title="item.title ?? item.label"
      :data-rail="item.label.toLowerCase()"
      :aria-current="active(item) ? 'page' : undefined"
      class="relative flex flex-col items-center gap-0.5 py-2.5 transition-colors"
      :class="active(item) ? 'text-fg' : 'text-faint hover:text-muted'"
    >
      <span
        v-if="active(item)"
        class="absolute inset-y-1.5 left-0 w-0.5 rounded-r bg-accent"
        aria-hidden="true"
      ></span>
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path v-for="(d, i) in item.paths" :key="i" :d="d" />
      </svg>
      <span class="text-[10px] leading-3">{{ item.label }}</span>
    </router-link>

    <!-- Bottom cluster: theme toggle + settings (the VS Code idiom). -->
    <div class="mt-auto flex flex-col items-stretch pb-1.5">
      <button
        type="button"
        class="flex flex-col items-center gap-0.5 py-2.5 text-faint transition-colors hover:text-muted"
        :title="theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'"
        aria-label="Toggle color theme"
        @click="toggleTheme"
      >
        <svg v-if="theme === 'dark'" width="20" height="20" viewBox="0 0 24 24" fill="none"
          stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
          <circle cx="12" cy="12" r="4" />
          <path d="M12 2v2M12 20v2m-7.07-2.93 1.41-1.41m11.32 0 1.41 1.41M2 12h2m16 0h2M4.93 4.93l1.41 1.41m11.32 0 1.41-1.41" />
        </svg>
        <svg v-else width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor"
          stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
        </svg>
      </button>
      <router-link
        :to="SETTINGS.to"
        :title="SETTINGS.label"
        data-rail="settings"
        :aria-current="active(SETTINGS) ? 'page' : undefined"
        class="relative flex flex-col items-center gap-0.5 py-2.5 transition-colors"
        :class="active(SETTINGS) ? 'text-fg' : 'text-faint hover:text-muted'"
      >
        <span
          v-if="active(SETTINGS)"
          class="absolute inset-y-1.5 left-0 w-0.5 rounded-r bg-accent"
          aria-hidden="true"
        ></span>
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor"
          stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path v-for="(d, i) in SETTINGS.paths" :key="i" :d="d" />
        </svg>
      </router-link>
    </div>
  </nav>
</template>
