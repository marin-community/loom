<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import * as api from '../api';
import type { SlackStatus } from '../types';
import { slackPath, firstBreak, pathVerdict, type LinkState } from '../lib/slackPath';
import { compactAge, exactTime } from '../lib/time';

// The Slack trigger path, link by link. A message becomes a session only if
// every link holds, and the ways this integration fails are mostly *late*
// links: a live socket carrying a person's token instead of the app's, or no
// repository to work on. A single connected/disconnected light reports health
// through all of them, so the path is the pane.
//
// Tokens live outside the settings registry (`LOOM_SLACK_APP_TOKEN` /
// `LOOM_SLACK_BOT_TOKEN` in the server environment) and are never rendered
// here — only whether they are set, and who Slack says they belong to.
const REFRESH_MS = 15_000;

const status = ref<SlackStatus | null>(null);
const error = ref('');
const loading = ref(true);
let timer: ReturnType<typeof setInterval> | undefined;

async function load() {
  try {
    status.value = await api.getSlackStatus();
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    loading.value = false;
  }
}

const links = computed(() => (status.value ? slackPath(status.value) : []));
const broken = computed(() => firstBreak(links.value));
const verdict = computed(() =>
  status.value ? pathVerdict(links.value) : { text: 'Checking…', tone: 'wait' as LinkState },
);
const socket = computed(() => status.value?.socket);

// Text as well as color, per the visual system: every state also has a word.
const TONE: Record<LinkState, { dot: string; text: string; word: string }> = {
  ok: { dot: 'bg-ok-line', text: 'text-ok', word: 'ready' },
  attn: { dot: 'bg-attn-line', text: 'text-attn', word: 'needs a look' },
  off: { dot: 'bg-faint/40', text: 'text-faint', word: 'off' },
  wait: { dot: 'bg-faint/40 animate-pulse', text: 'text-muted', word: 'checking' },
};

// The counters only mean something once the socket has been up, and a quiet run
// is normal — so they are a footer, not a headline.
const traffic = computed(() => {
  const s = socket.value;
  if (!s || s.state !== 'connected') return '';
  const last = s.last_event_at ? `last ${compactAge(s.last_event_at)} ago` : 'none yet';
  return `${s.events_received} received · ${s.sessions_launched} launched · ${last}`;
});

onMounted(() => {
  load();
  // Socket state changes without anyone touching this page — a Slack refresh, a
  // dropped connection, the first mention of the day. Poll while the pane is
  // open so what it shows stays true.
  timer = setInterval(load, REFRESH_MS);
});
onUnmounted(() => clearInterval(timer));
</script>

<template>
  <div>
    <h2 class="text-2xs font-semibold uppercase tracking-wider text-muted mb-1.5">Slack</h2>
    <div class="overflow-hidden rounded-md border border-line bg-surface" data-testid="slack-panel">
      <p v-if="error" class="px-3 py-2.5 text-sm text-block">{{ error }}</p>

      <template v-else>
        <!-- Verdict: the diagnosis before the detail. -->
        <div class="flex items-baseline gap-2 border-b border-line px-3 py-2.5">
          <span
            class="h-1.5 w-1.5 shrink-0 rounded-full"
            :class="TONE[verdict.tone].dot"
            aria-hidden="true"
          ></span>
          <p
            class="text-sm font-medium"
            :class="TONE[verdict.tone].text"
            data-testid="slack-verdict"
          >
            {{ verdict.text }}
          </p>
          <p v-if="broken?.fix" class="min-w-0 flex-1 text-xs text-muted">{{ broken.fix }}</p>
        </div>

        <!-- The path: one row per check the server makes, in the order it makes
             them, joined by a hairline down the marker gutter so the sequence
             reads as a sequence rather than six independent lights. -->
        <ol v-if="links.length" class="px-3 py-2">
          <li
            v-for="(l, i) in links"
            :key="l.key"
            class="relative flex items-baseline gap-2.5 py-1 pl-4"
            :data-testid="`slack-link-${l.key}`"
          >
            <span
              v-if="i < links.length - 1"
              class="absolute bottom-[-0.25rem] left-[3px] top-[0.85rem] w-px bg-line"
              aria-hidden="true"
            ></span>
            <span
              class="absolute left-0 top-[0.5rem] h-[7px] w-[7px] rounded-full ring-2 ring-surface"
              :class="TONE[l.state].dot"
              aria-hidden="true"
            ></span>
            <span class="w-28 shrink-0 text-xs" :class="l === broken ? 'text-fg' : 'text-muted'">
              {{ l.label }}
            </span>
            <span class="min-w-0 flex-1 font-mono text-2xs" :class="TONE[l.state].text">
              {{ l.detail }}
            </span>
            <span class="sr-only">{{ TONE[l.state].word }}</span>
          </li>
        </ol>
        <p v-else-if="loading" class="px-3 py-2.5 text-sm text-muted">Checking…</p>

        <!-- What the socket has actually done. A skipped trigger is this
             integration's quietest failure, so it stays on the page instead of
             only in the log. -->
        <div v-if="traffic || socket?.last_skip" class="border-t border-line px-3 py-2">
          <p v-if="traffic" class="font-mono text-2xs text-faint">{{ traffic }}</p>
          <p
            v-if="socket?.last_skip"
            class="mt-0.5 text-2xs text-attn"
            data-testid="slack-last-skip"
          >
            Last skipped trigger: {{ socket.last_skip
            }}<template v-if="socket.last_skip_at">
              (<time :datetime="socket.last_skip_at" :title="exactTime(socket.last_skip_at)"
                >{{ compactAge(socket.last_skip_at) }} ago</time
              >)</template
            >
          </p>
        </div>
      </template>
    </div>
  </div>
</template>
