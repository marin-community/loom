<script setup lang="ts">
import { computed, watch } from 'vue';
import AppRail from './components/AppRail.vue';
import StatusBar from './components/StatusBar.vue';
import { me } from './auth';
import { useFleet } from './lib/sessionsStore';

// The workbench shell (docs/loom-ui.md): nav rail on the left, a thin status
// bar pinned to the bottom, and the view filling everything between — no top
// app bar. `main` is a flex column so a full-height view (session detail,
// file browser) can `flex-1 min-h-0` to fill it exactly, while list views
// simply grow and let `main` scroll.
//
// Until the caller is authenticated we render the view bare (no rail/status
// bar) — the router guard keeps that view on the login screen.
const authed = computed(() => me.authenticated);

// One fleet poll for the whole app, owned here at the shell. The session list,
// the status bar, and the detail page all read the same shared snapshot
// (lib/sessionsStore) instead of each polling /api/sessions on their own — so a
// view paints from cache the instant it mounts (no empty-state flash, no
// refetch round-trip) and there is exactly one request per tick. Start it once
// authenticated; stop on sign-out so the login screen never polls.
const { startFleetPoll, stopFleetPoll } = useFleet();
watch(authed, (ok) => (ok ? startFleetPoll() : stopFleetPoll()), { immediate: true });

// Views kept alive across navigation so returning is instant — no remount, no
// refetch flash, no entrance-animation replay. The list views return exactly as
// left (scroll, filter, the open create form); the session detail page returns
// to its warm terminal (scrollback intact, no reconnect). Chat is NOT cached —
// it is cheap to remount.
const CACHED_VIEWS = ['SessionList', 'Issues', 'Watches', 'SessionDetail'];

// Cache key per cached instance. List views are singletons (keyed by their
// stable path); the session detail is keyed per session id so each session gets
// its own instance — flipping to a *different* session never reuses the wrong
// terminal. `:max` (below) bounds the LRU. What a cached detail holds open while
// off-screen is its terminal *WebSocket* (kept warm on purpose — that's the
// snappiness win, and WebSockets ride a separate connection pool from HTTP); its
// status SSEs are paused on deactivate (see SessionDetail/SessionConversation),
// so idle EventSources don't accumulate against the browser's ~6 per-origin
// HTTP/1.1 cap. max=3 therefore bounds warm terminals — memory, the xterm WebGL
// contexts (browsers cap those too), and server-side PTYs — keeping the common
// set warm (the fleet plus the current session, with headroom) while older
// details evict and tear down their socket. An evicted list remounts instantly
// from the store cache, so list eviction is invisible — the cap is really there
// to bound the detail terminals.
const KEEP_ALIVE_MAX = 3;
function cacheKey(route: { path: string; params: Record<string, string | string[]> }): string {
  // Every `/s/:id…` path (the work tabs and the Artifacts deep-links) is the
  // same SessionDetail instance, so they share one cache key — switching to
  // artifacts and back is a tab flip on the warm page, never a remount.
  const id = route.params.id;
  if (typeof id === 'string' && route.path.startsWith(`/s/${id}`)) return `s:${id}`;
  return route.path;
}
</script>

<template>
  <router-view v-if="!authed" />
  <div v-else class="flex h-screen overflow-hidden bg-canvas font-sans text-fg">
    <AppRail />
    <div class="flex min-w-0 flex-1 flex-col">
      <main class="flex min-h-0 flex-1 flex-col overflow-auto">
        <router-view v-slot="{ Component, route }">
          <keep-alive :include="CACHED_VIEWS" :max="KEEP_ALIVE_MAX">
            <component :is="Component" :key="cacheKey(route)" />
          </keep-alive>
        </router-view>
      </main>
      <StatusBar />
    </div>
  </div>
</template>
