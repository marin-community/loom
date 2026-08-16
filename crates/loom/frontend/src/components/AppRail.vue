<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { me } from '../auth';
import { theme, toggleTheme } from '../theme';
import {
  useMobileSessionNavigation,
  type MobileSessionSurface,
} from '../lib/mobileSessionNavigation';
import ScratchPanel from './ScratchPanel.vue';

// The workbench nav rail — the app's only chrome besides the status bar
// (see docs/loom-ui.md). Icon+label items down the left edge; the active view
// carries a 2px accent bar on its left (the VS Code activity-bar idiom).
// Settings and the theme toggle pin to the bottom.
const route = useRoute();

interface RailItem {
  to: string;
  /** The short rail caption (the rail is 56px — long names don't fit). */
  label: string;
  /** Tooltip; defaults to the label. Lets "Watch" expand to "Watches". */
  title?: string;
  /** Active when the current path matches one of these prefixes ('/' is exact,
   *  plus the session pages which drill down from the fleet list). */
  match: (path: string) => boolean;
  /** Inline SVG path data (lucide outlines, 24px grid, stroked). */
  paths: string[];
  data?: string;
}

const MAIN: RailItem[] = [
  {
    to: '/',
    label: 'Sessions',
    match: (p) => p === '/' || p.startsWith('/s/') || p === '/sessions/new',
    // square-terminal — a session is a live agent terminal.
    paths: [
      'm7 11 2-2-2-2',
      'M13 15h4',
      'M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Z',
    ],
  },
  {
    to: '/channels',
    label: 'Channels',
    title: 'Channels — durable user and agent messages',
    data: 'channels',
    match: (p) => p.startsWith('/channels'),
    // inbox — a compact, durable communication stream.
    paths: ['M4 4h16v16H4z', 'M4 14h4l2 3h4l2-3h4', 'M8 8h8', 'M8 11h6'],
  },
  {
    to: '/issues',
    label: 'Backlog',
    title: 'Backlog — explicit work items and GitHub issues',
    data: 'issues',
    match: (p) => p.startsWith('/issues'),
    // circle-dot — the issue-tracker glyph.
    paths: ['M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Z', 'M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2Z'],
  },
  {
    to: '/watches',
    label: 'Watch',
    title: 'Watches — watch agents over the fleet',
    match: (p) => p.startsWith('/watches'),
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
const visibleMain = computed(() =>
  MAIN.filter((item) => item.to !== '/shell' || me.role === 'admin'),
);

interface SessionItem {
  surface: MobileSessionSurface;
  label: string;
  paths: string[];
}

const SESSION_ITEMS: SessionItem[] = [
  {
    surface: 'conversation',
    label: 'Chat',
    // message-square-text
    paths: ['M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4Z', 'M8 8h8', 'M8 12h6'],
  },
  {
    surface: 'artifacts',
    label: 'Artifacts',
    // files
    paths: [
      'M15 2H6a2 2 0 0 0-2 2v13',
      'M14 2v6h6',
      'M8 7h4',
      'M8 11h8',
      'M8 15h8',
      'M8 22h10a2 2 0 0 0 2-2V8l-6-6',
    ],
  },
];

const MORE_PATHS = ['M5 12h.01', 'M12 12h.01', 'M19 12h.01'];
const mobileSessionNavigation = useMobileSessionNavigation();
const sessionMode = computed(() => {
  const id = route.params.id;
  return (
    typeof id === 'string' &&
    route.path.startsWith(`/s/${id}`) &&
    mobileSessionNavigation.value?.id === id
  );
});
const mobileMain = computed(() => MAIN.filter((item) => item.to !== '/shell'));
const mobileMoreMain = computed(() => mobileMain.value.filter((item) => item.to !== '/'));
const mobileMoreActive = computed(
  () =>
    (sessionMode.value &&
      ['terminal', 'changes', 'shells'].includes(mobileSessionNavigation.value?.active ?? '')) ||
    (!sessionMode.value && (route.path.startsWith('/settings') || route.path.startsWith('/shell'))),
);

const moreOpen = ref(false);
const moreButton = ref<HTMLButtonElement | null>(null);
const morePanel = ref<HTMLElement | null>(null);
const moreFocusable =
  'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

function moreFocusTargets(): HTMLElement[] {
  return Array.from(morePanel.value?.querySelectorAll<HTMLElement>(moreFocusable) ?? []);
}

async function toggleMore() {
  moreOpen.value = !moreOpen.value;
  if (!moreOpen.value) return;
  await nextTick();
  moreFocusTargets()[0]?.focus();
}

function closeMore(restoreFocus = false) {
  if (!moreOpen.value) return;
  moreOpen.value = false;
  if (restoreFocus) void nextTick(() => moreButton.value?.focus());
}

function selectSessionSurface(surface: MobileSessionSurface) {
  void mobileSessionNavigation.value?.select(surface);
  closeMore();
}

function openSessionDetails() {
  mobileSessionNavigation.value?.openDetails();
  closeMore();
}

function onMoreKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    event.preventDefault();
    closeMore(true);
    return;
  }
  if (event.key !== 'Tab') return;
  const targets = moreFocusTargets();
  if (!targets.length) {
    event.preventDefault();
    return;
  }
  const first = targets[0];
  const last = targets[targets.length - 1];
  const focused = document.activeElement;
  if (event.shiftKey && (focused === first || !morePanel.value?.contains(focused))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && focused === last) {
    event.preventDefault();
    first.focus();
  }
}

watch(
  () => route.fullPath,
  () => closeMore(),
);
watch(sessionMode, () => closeMore());
onBeforeUnmount(() => closeMore());
</script>

<template>
  <!-- Phone navigation is contextual. Fleet pages keep four durable global
       destinations; an open session replaces them with its warm work surfaces.
       Secondary session/global actions live in the thumb-reachable More sheet. -->
  <nav
    class="relative z-50 order-2 flex h-14 w-full shrink-0 flex-row items-stretch border-t border-line bg-rail sm:hidden"
    aria-label="Primary"
    data-testid="mobile-primary-nav"
  >
    <button
      v-if="moreOpen"
      type="button"
      class="fixed inset-0 bottom-14 z-40 cursor-default bg-black/20"
      aria-label="Close More menu"
      @click="closeMore(true)"
    ></button>
    <section
      v-if="moreOpen"
      id="mobile-more-menu"
      ref="morePanel"
      class="fixed bottom-14 left-0 right-0 z-50 max-h-[min(34rem,72dvh)] overflow-y-auto border-t border-line bg-surface p-3 shadow-2xl"
      role="dialog"
      aria-modal="true"
      aria-label="More navigation and session actions"
      data-testid="mobile-more-menu"
      @keydown="onMoreKeydown"
    >
      <div v-if="sessionMode" class="mb-3 border-b border-line pb-3">
        <p class="mb-2 text-2xs font-semibold uppercase tracking-wider text-muted">Session</p>
        <div class="grid grid-cols-2 gap-2">
          <button
            type="button"
            class="min-h-11 rounded border border-line bg-input px-3 py-2 text-left text-sm text-fg"
            data-testid="mobile-session-details"
            @click="openSessionDetails"
          >
            <span class="block font-medium">Details & actions</span>
            <span class="block text-2xs text-faint">Status, links, lifecycle</span>
          </button>
          <button
            type="button"
            class="min-h-11 rounded border border-line bg-input px-3 py-2 text-left text-sm text-fg"
            data-testid="mobile-session-changes"
            @click="selectSessionSurface('changes')"
          >
            <span class="block font-medium">Changes</span>
            <span class="block text-2xs text-faint">Review the branch diff</span>
          </button>
          <button
            v-if="mobileSessionNavigation?.protocol === 'terminal'"
            type="button"
            class="min-h-11 rounded border border-line bg-input px-3 py-2 text-left text-sm text-fg"
            data-testid="mobile-session-agent"
            @click="selectSessionSurface('terminal')"
          >
            <span class="block font-medium">Agent terminal</span>
            <span class="block text-2xs text-faint">Interactive session surface</span>
          </button>
          <button
            v-if="mobileSessionNavigation?.protocol === 'acp'"
            type="button"
            class="min-h-11 rounded border border-line bg-input px-3 py-2 text-left text-sm text-fg"
            data-testid="mobile-session-shells"
            @click="selectSessionSurface('shells')"
          >
            <span class="block font-medium">Shells</span>
            <span class="block text-2xs text-faint">Worktree escape hatch</span>
          </button>
        </div>
        <div class="mt-3 rounded border border-line bg-input p-2">
          <p class="mb-2 text-2xs font-semibold uppercase tracking-wider text-muted">Scratch</p>
          <ScratchPanel
            v-if="mobileSessionNavigation"
            :id="mobileSessionNavigation.id"
            test-id="mobile-scratch-panel"
            embedded
          />
        </div>
      </div>

      <div class="mb-3">
        <p class="mb-2 text-2xs font-semibold uppercase tracking-wider text-muted">Navigate</p>
        <div class="grid grid-cols-2 gap-2">
          <router-link
            v-for="item in sessionMode ? mobileMoreMain : mobileMain"
            :key="item.to"
            :to="item.to"
            class="flex min-h-11 items-center rounded border border-line bg-input px-3 text-sm text-fg"
          >
            {{ item.label }}
          </router-link>
          <router-link
            v-if="me.role === 'admin'"
            to="/shell"
            class="flex min-h-11 items-center rounded border border-line bg-input px-3 text-sm text-fg"
          >
            Shell
          </router-link>
        </div>
      </div>

      <div class="border-t border-line pt-3">
        <p class="mb-2 text-2xs font-semibold uppercase tracking-wider text-muted">Preferences</p>
        <div class="grid grid-cols-2 gap-2">
          <button
            type="button"
            class="min-h-11 rounded border border-line bg-input px-3 text-left text-sm text-fg"
            @click="toggleTheme"
          >
            {{ theme === 'dark' ? 'Light theme' : 'Dark theme' }}
          </button>
          <router-link
            to="/settings"
            class="flex min-h-11 items-center rounded border border-line bg-input px-3 text-sm text-fg"
          >
            Settings
          </router-link>
        </div>
      </div>
    </section>

    <template v-if="sessionMode">
      <router-link
        to="/"
        data-mobile-nav="sessions"
        class="relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 text-faint transition-colors hover:text-muted"
      >
        <svg
          width="21"
          height="21"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path v-for="(d, i) in MAIN[0].paths" :key="i" :d="d" />
        </svg>
        <span class="text-[10px] leading-3">Sessions</span>
      </router-link>
      <button
        v-for="item in SESSION_ITEMS"
        :key="item.surface"
        type="button"
        :data-mobile-nav="item.surface"
        :aria-current="mobileSessionNavigation?.active === item.surface ? 'page' : undefined"
        class="relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 transition-colors"
        :class="
          mobileSessionNavigation?.active === item.surface
            ? 'text-fg'
            : 'text-faint hover:text-muted'
        "
        @click="selectSessionSurface(item.surface)"
      >
        <span
          v-if="mobileSessionNavigation?.active === item.surface"
          class="absolute inset-x-2 top-0 h-0.5 rounded-b bg-accent"
          aria-hidden="true"
        ></span>
        <svg
          width="21"
          height="21"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path v-for="(d, i) in item.paths" :key="i" :d="d" />
        </svg>
        <span class="text-[10px] leading-3">{{ item.label }}</span>
      </button>
    </template>
    <template v-else>
      <router-link
        v-for="item in mobileMain"
        :key="item.to"
        :to="item.to"
        :data-rail="item.data ?? item.label.toLowerCase()"
        :aria-current="active(item) ? 'page' : undefined"
        class="relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 transition-colors"
        :class="active(item) ? 'text-fg' : 'text-faint hover:text-muted'"
      >
        <span
          v-if="active(item)"
          class="absolute inset-x-2 top-0 h-0.5 rounded-b bg-accent"
          aria-hidden="true"
        ></span>
        <svg
          width="21"
          height="21"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path v-for="(d, i) in item.paths" :key="i" :d="d" />
        </svg>
        <span class="text-[10px] leading-3">{{ item.label }}</span>
      </router-link>
    </template>

    <button
      ref="moreButton"
      type="button"
      class="relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 transition-colors"
      :class="moreOpen || mobileMoreActive ? 'text-fg' : 'text-faint hover:text-muted'"
      :aria-expanded="moreOpen"
      aria-controls="mobile-more-menu"
      data-mobile-nav="more"
      @click="toggleMore"
    >
      <span
        v-if="moreOpen || mobileMoreActive"
        class="absolute inset-x-2 top-0 h-0.5 rounded-b bg-accent"
        aria-hidden="true"
      ></span>
      <svg
        width="21"
        height="21"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        aria-hidden="true"
      >
        <path v-for="(d, i) in MORE_PATHS" :key="i" :d="d" />
      </svg>
      <span class="text-[10px] leading-3">More</span>
    </button>
  </nav>

  <!-- Desktop keeps the stable global activity rail. -->
  <nav
    class="hidden w-14 shrink-0 flex-col items-stretch border-r border-line bg-rail sm:flex"
    aria-label="Primary"
  >
    <!-- Wordmark — a warp/weft weave glyph; home link to the fleet. -->
    <router-link
      to="/"
      class="hidden h-12 items-center justify-center text-accent sm:flex"
      title="loom — agent sessions"
      aria-label="loom home"
    >
      <svg
        width="22"
        height="22"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.75"
        stroke-linecap="round"
        aria-hidden="true"
      >
        <path d="M4 9h16M4 15h16M9 4v16M15 4v16" />
      </svg>
    </router-link>

    <router-link
      v-for="item in visibleMain"
      :key="item.to"
      :to="item.to"
      :title="item.title ?? item.label"
      :data-rail="item.data ?? item.label.toLowerCase()"
      :aria-current="active(item) ? 'page' : undefined"
      class="relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 transition-colors sm:flex-none sm:px-0 sm:py-2.5"
      :class="active(item) ? 'text-fg' : 'text-faint hover:text-muted'"
    >
      <span
        v-if="active(item)"
        class="absolute inset-x-2 top-0 h-0.5 rounded-b bg-accent sm:inset-x-auto sm:inset-y-1.5 sm:left-0 sm:top-auto sm:h-auto sm:w-0.5 sm:rounded-b-none sm:rounded-r"
        aria-hidden="true"
      ></span>
      <svg
        width="22"
        height="22"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path v-for="(d, i) in item.paths" :key="i" :d="d" />
      </svg>
      <span class="text-[9px] leading-3 sm:text-[10px]">{{ item.label }}</span>
    </router-link>

    <!-- Bottom cluster: theme toggle + settings (the VS Code idiom). -->
    <div
      class="flex min-w-0 flex-[2] flex-row items-stretch sm:mt-auto sm:flex-none sm:flex-col sm:pb-1.5"
    >
      <button
        type="button"
        class="flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 text-faint transition-colors hover:text-muted sm:flex-none sm:px-0 sm:py-2.5"
        :title="theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'"
        aria-label="Toggle color theme"
        @click="toggleTheme"
      >
        <svg
          v-if="theme === 'dark'"
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          aria-hidden="true"
        >
          <circle cx="12" cy="12" r="4" />
          <path
            d="M12 2v2M12 20v2m-7.07-2.93 1.41-1.41m11.32 0 1.41 1.41M2 12h2m16 0h2M4.93 4.93l1.41 1.41m11.32 0 1.41-1.41"
          />
        </svg>
        <svg
          v-else
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
        </svg>
        <span class="text-[9px] leading-3 sm:hidden">Theme</span>
      </button>
      <router-link
        :to="SETTINGS.to"
        :title="SETTINGS.label"
        data-rail="settings"
        :aria-current="active(SETTINGS) ? 'page' : undefined"
        class="relative flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 px-0.5 py-1 transition-colors sm:flex-none sm:px-0 sm:py-2.5"
        :class="active(SETTINGS) ? 'text-fg' : 'text-faint hover:text-muted'"
      >
        <span
          v-if="active(SETTINGS)"
          class="absolute inset-x-2 top-0 h-0.5 rounded-b bg-accent sm:inset-x-auto sm:inset-y-1.5 sm:left-0 sm:top-auto sm:h-auto sm:w-0.5 sm:rounded-b-none sm:rounded-r"
          aria-hidden="true"
        ></span>
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path v-for="(d, i) in SETTINGS.paths" :key="i" :d="d" />
        </svg>
        <span class="text-[9px] leading-3 sm:hidden">Settings</span>
      </router-link>
    </div>
  </nav>
</template>
