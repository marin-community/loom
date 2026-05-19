<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import { get, post, patch, del } from '../api';
import type { Workspace, WeaverEvent, DiffStat } from '../types';
import StatusBadge from '../components/StatusBadge.vue';

const props = defineProps<{ id: string }>();
const router = useRouter();

const ws = ref<Workspace | null>(null);
const screen = ref('');
const events = ref<WeaverEvent[]>([]);
const error = ref('');
const notice = ref('');

const goalDraft = ref('');
const descDraft = ref('');
const sendText = ref('');

const diff = ref<{ stat: DiffStat; patch: string } | null>(null);
const busy = ref('');

const screenBox = ref<HTMLElement | null>(null);
let source: EventSource | null = null;

async function loadWorkspace() {
  ws.value = (await get(`/workspaces/${props.id}`)) as Workspace;
  goalDraft.value = ws.value.goal;
  descDraft.value = ws.value.description;
}

async function loadAll() {
  try {
    await loadWorkspace();
    const pane = (await get(`/workspaces/${props.id}/pane`)) as { content: string };
    screen.value = pane.content;
    events.value = (await get(`/workspaces/${props.id}/log`)) as WeaverEvent[];
    await scrollScreen();
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function scrollScreen() {
  await nextTick();
  if (screenBox.value) screenBox.value.scrollTop = screenBox.value.scrollHeight;
}

function openStream() {
  source = new EventSource(`/api/workspaces/${props.id}/events`);
  source.addEventListener('screen', (e) => {
    const ev = JSON.parse((e as MessageEvent).data) as WeaverEvent;
    screen.value = String(ev.data.content ?? '');
    scrollScreen();
  });
  for (const kind of ['status', 'summary', 'note']) {
    source.addEventListener(kind, (e) => {
      const ev = JSON.parse((e as MessageEvent).data) as WeaverEvent;
      events.value.push(ev);
      loadWorkspace().catch(() => {});
    });
  }
}

async function act(name: string, fn: () => Promise<void>) {
  busy.value = name;
  error.value = '';
  notice.value = '';
  try {
    await fn();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = '';
  }
}

const saveGoal = () =>
  act('goal', async () => {
    await patch(`/workspaces/${props.id}`, { goal: goalDraft.value });
    notice.value = 'Goal saved.';
    await loadWorkspace();
  });

const saveDesc = () =>
  act('desc', async () => {
    await patch(`/workspaces/${props.id}`, { description: descDraft.value });
    notice.value = 'Description saved.';
    await loadWorkspace();
  });

const send = () =>
  act('send', async () => {
    if (!sendText.value.trim()) return;
    await post(`/workspaces/${props.id}/send`, { text: sendText.value });
    sendText.value = '';
  });

const summarize = () =>
  act('summary', async () => {
    const res = (await post(`/workspaces/${props.id}/summarize`)) as { description: string };
    descDraft.value = res.description;
    notice.value = 'Summary updated.';
    await loadWorkspace();
  });

const loadDiff = () =>
  act('diff', async () => {
    diff.value = (await get(`/workspaces/${props.id}/diff`)) as {
      stat: DiffStat;
      patch: string;
    };
  });

const merge = () =>
  act('merge', async () => {
    if (!confirm('Merge this workspace branch into its base branch?')) return;
    const res = (await post(`/workspaces/${props.id}/merge`)) as { branch: string };
    notice.value = `Merged ${res.branch}.`;
    await loadWorkspace();
  });

const remove = () =>
  act('remove', async () => {
    if (!confirm('Remove this workspace, its worktree and tmux session?')) return;
    await del(`/workspaces/${props.id}`);
    router.push('/');
  });

function eventLine(ev: WeaverEvent): string {
  const d = ev.data || {};
  if (ev.kind === 'status') return `status → ${d.status ?? '?'}`;
  if (ev.kind === 'summary') return `summary: ${d.description ?? ''}`;
  if (ev.kind === 'note') return String(d.text ?? '');
  return ev.kind;
}

onMounted(() => {
  loadAll();
  openStream();
});
onUnmounted(() => source?.close());
</script>

<template>
  <div v-if="ws">
    <div class="flex items-center gap-3 mb-1">
      <router-link to="/" class="text-neutral-500 hover:text-neutral-300 text-sm">← all</router-link>
      <h1 class="text-xl font-semibold">{{ ws.name }}</h1>
      <StatusBadge :status="ws.status" />
    </div>
    <div class="text-xs text-neutral-600 font-mono mb-1">
      {{ ws.id }} · {{ ws.branch }} (base {{ ws.base_branch }}) · {{ ws.agent_kind }} · {{ ws.tmux_session }}
      <span v-if="ws.github_issue"> · {{ ws.github_repo }}#{{ ws.github_issue }}</span>
    </div>
    <div class="text-xs text-neutral-600 font-mono mb-4">worktree: {{ ws.work_dir }}</div>

    <p v-if="error" class="mb-3 text-sm text-red-400">{{ error }}</p>
    <p v-if="notice" class="mb-3 text-sm text-emerald-400">{{ notice }}</p>

    <div class="grid gap-5 lg:grid-cols-3">
      <!-- Left: goal, description, actions -->
      <div class="space-y-5 lg:col-span-1">
        <section class="rounded border border-neutral-800 bg-neutral-900 p-4">
          <label class="block text-xs text-neutral-400 mb-1">Goal</label>
          <textarea
            v-model="goalDraft"
            rows="3"
            class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none"
          ></textarea>
          <button
            class="mt-2 rounded bg-neutral-700 hover:bg-neutral-600 px-2 py-1 text-xs"
            :disabled="busy === 'goal'"
            @click="saveGoal"
          >
            Save goal
          </button>
        </section>

        <section class="rounded border border-neutral-800 bg-neutral-900 p-4">
          <div class="flex items-center justify-between mb-1">
            <label class="text-xs text-neutral-400">Description / current state</label>
            <button
              class="rounded bg-neutral-700 hover:bg-neutral-600 px-2 py-0.5 text-xs"
              :disabled="busy === 'summary'"
              @click="summarize"
            >
              {{ busy === 'summary' ? 'Summarizing…' : 'Summarize now' }}
            </button>
          </div>
          <textarea
            v-model="descDraft"
            rows="6"
            class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none"
          ></textarea>
          <button
            class="mt-2 rounded bg-neutral-700 hover:bg-neutral-600 px-2 py-1 text-xs"
            :disabled="busy === 'desc'"
            @click="saveDesc"
          >
            Save description
          </button>
        </section>

        <section class="rounded border border-neutral-800 bg-neutral-900 p-4 flex gap-2">
          <button
            class="rounded bg-indigo-700 hover:bg-indigo-600 px-3 py-1.5 text-sm"
            :disabled="busy === 'merge'"
            @click="merge"
          >
            Merge
          </button>
          <button
            class="rounded bg-red-800 hover:bg-red-700 px-3 py-1.5 text-sm"
            :disabled="busy === 'remove'"
            @click="remove"
          >
            Remove
          </button>
        </section>
      </div>

      <!-- Right: live screen, send box, events, diff -->
      <div class="space-y-5 lg:col-span-2">
        <section class="rounded border border-neutral-800 bg-neutral-900 p-4">
          <div class="text-xs text-neutral-400 mb-2">Live agent screen</div>
          <pre
            ref="screenBox"
            class="h-72 overflow-auto rounded bg-black p-3 text-xs leading-snug text-neutral-200 whitespace-pre-wrap"
          >{{ screen || '(no output yet)' }}</pre>
          <form class="mt-2 flex gap-2" @submit.prevent="send">
            <input
              v-model="sendText"
              placeholder="Send a line to the agent…"
              class="flex-1 rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none"
            />
            <button
              type="submit"
              class="rounded bg-emerald-700 hover:bg-emerald-600 px-3 py-1.5 text-sm"
              :disabled="busy === 'send'"
            >
              Send
            </button>
          </form>
        </section>

        <section class="rounded border border-neutral-800 bg-neutral-900 p-4">
          <div class="text-xs text-neutral-400 mb-2">Activity</div>
          <ul class="space-y-1 text-sm max-h-60 overflow-auto">
            <li v-for="ev in events" :key="ev.id" class="flex gap-2">
              <span class="text-neutral-600 font-mono text-xs shrink-0">
                {{ ev.created_at.slice(11, 19) }}
              </span>
              <span class="text-neutral-300">{{ eventLine(ev) }}</span>
            </li>
            <li v-if="!events.length" class="text-neutral-600">No activity yet.</li>
          </ul>
        </section>

        <section class="rounded border border-neutral-800 bg-neutral-900 p-4">
          <div class="flex items-center justify-between mb-2">
            <span class="text-xs text-neutral-400">Diff vs {{ ws.base_branch }}</span>
            <button
              class="rounded bg-neutral-700 hover:bg-neutral-600 px-2 py-0.5 text-xs"
              :disabled="busy === 'diff'"
              @click="loadDiff"
            >
              {{ busy === 'diff' ? 'Loading…' : 'Load diff' }}
            </button>
          </div>
          <div v-if="diff">
            <p class="text-xs text-neutral-500 mb-2">
              {{ diff.stat.files_changed }} files ·
              <span class="text-emerald-400">+{{ diff.stat.insertions }}</span> ·
              <span class="text-red-400">-{{ diff.stat.deletions }}</span>
            </p>
            <pre
              class="max-h-96 overflow-auto rounded bg-black p-3 text-xs text-neutral-300 whitespace-pre-wrap"
            >{{ diff.patch || '(no changes)' }}</pre>
          </div>
        </section>
      </div>
    </div>
  </div>
  <p v-else class="text-neutral-500">Loading…</p>
</template>
