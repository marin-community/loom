<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { useRouter } from 'vue-router';
import { get, post, patch, del } from '../api';
import type { Session, WeaverEvent, Issue } from '../types';
import StatusBadge from '../components/StatusBadge.vue';
import AttentionBadge from '../components/AttentionBadge.vue';
import AgentTerminal from '../components/AgentTerminal.vue';
import ScratchPanel from '../components/ScratchPanel.vue';

const props = defineProps<{ id: string }>();
const router = useRouter();

const ws = ref<Session | null>(null);
const events = ref<WeaverEvent[]>([]);
const issues = ref<Issue[]>([]);
const backlog = ref<Issue[]>([]);
const error = ref('');
const notice = ref('');

const titleDraft = ref('');
const goalDraft = ref('');
const descDraft = ref('');
const attnLevelDraft = ref('ok');
const attnNoteDraft = ref('');

const busy = ref('');

let source: EventSource | null = null;

async function loadSession() {
  ws.value = (await get(`/sessions/${props.id}`)) as Session;
  titleDraft.value = ws.value.branch.title;
  goalDraft.value = ws.value.branch.goal;
  descDraft.value = ws.value.branch.description;
  attnLevelDraft.value = ws.value.branch.attention || 'ok';
  attnNoteDraft.value = ws.value.branch.attention_note;
}

async function loadIssues() {
  if (!ws.value) return;
  // The session's own claimed work, plus the repo's unclaimed backlog.
  try {
    issues.value = (await get(`/branches/${ws.value.branch.id}/issues`)) as Issue[];
  } catch {
    // Issues are read-only here; failure is non-fatal for the view.
  }
  try {
    const repo = encodeURIComponent(ws.value.branch.repo_root);
    backlog.value = (await get(`/repos/issues?repo_root=${repo}&scope=backlog`)) as Issue[];
  } catch {
    // Backlog is best-effort context; ignore failures.
  }
}

async function loadAll() {
  try {
    await loadSession();
    events.value = (await get(`/sessions/${props.id}/log`)) as WeaverEvent[];
    await loadIssues();
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

const saveAttention = () =>
  act('attention', async () => {
    await patch(`/sessions/${props.id}`, {
      attention: attnLevelDraft.value,
      attention_note: attnNoteDraft.value,
    });
    notice.value = 'Attention updated.';
    await loadSession();
  });

function openStream() {
  source = new EventSource(`/api/sessions/${props.id}/events`);
  for (const kind of ['status', 'attention', 'summary', 'note']) {
    source.addEventListener(kind, (e) => {
      const ev = JSON.parse((e as MessageEvent).data) as WeaverEvent;
      events.value.push(ev);
      loadSession().catch(() => {});
    });
  }
  for (const kind of ['issue_added', 'issue_closed', 'issue_reopened']) {
    source.addEventListener(kind, () => {
      loadIssues().catch(() => {});
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

const saveTitle = () =>
  act('title', async () => {
    await patch(`/sessions/${props.id}`, { title: titleDraft.value });
    notice.value = 'Title saved.';
    await loadSession();
  });

const saveGoal = () =>
  act('goal', async () => {
    await patch(`/sessions/${props.id}`, { goal: goalDraft.value });
    notice.value = 'Goal saved.';
    await loadSession();
  });

const saveDesc = () =>
  act('desc', async () => {
    await patch(`/sessions/${props.id}`, { description: descDraft.value });
    notice.value = 'Description saved.';
    await loadSession();
  });

const summarize = () =>
  act('summary', async () => {
    const res = (await post(`/sessions/${props.id}/summarize`)) as { description: string };
    descDraft.value = res.description;
    notice.value = 'Summary updated.';
    await loadSession();
  });

const merge = () =>
  act('merge', async () => {
    if (!confirm('Merge this branch into its base branch?')) return;
    const res = (await post(`/sessions/${props.id}/merge`)) as { branch: string };
    notice.value = `Merged ${res.branch}.`;
    await loadSession();
  });

const remove = () =>
  act('remove', async () => {
    if (!confirm('Remove this session, its worktree and tmux session?')) return;
    await del(`/sessions/${props.id}`);
    router.push('/');
  });

const adopt = () =>
  act('adopt', async () => {
    await post(`/sessions/${props.id}/adopt`);
    notice.value = 'Session adopted — tmux session recreated.';
    await loadSession();
  });

function eventLine(ev: WeaverEvent): string {
  const d = ev.data || {};
  if (ev.kind === 'status') return `status → ${d.status ?? '?'}`;
  if (ev.kind === 'attention')
    return `attention → ${d.level ?? '?'}${d.note ? ` (${d.note})` : ''}`;
  if (ev.kind === 'summary') return `summary: ${d.description ?? ''}`;
  if (ev.kind === 'note') return String(d.text ?? '');
  if (ev.kind === 'issue_added') return `issue added: ${d.title ?? ''}`;
  if (ev.kind === 'issue_closed') return `issue closed: #${d.id ?? '?'}`;
  if (ev.kind === 'issue_reopened') return `issue reopened: #${d.id ?? '?'}`;
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
      <router-link to="/" class="text-muted hover:text-muted text-sm">← all</router-link>
      <h1 class="text-xl font-semibold">{{ ws.branch.title || ws.branch.name }}</h1>
      <AttentionBadge :level="ws.branch.attention || 'ok'" :note="ws.branch.attention_note" />
      <StatusBadge :status="ws.status" />
    </div>
    <p
      v-if="ws.branch.attention_note"
      class="text-sm text-muted mb-1"
      data-testid="attention-note"
    >
      {{ ws.branch.attention_note }}
    </p>
    <div class="text-xs text-faint font-mono mb-1">
      {{ ws.id }} · {{ ws.branch.branch }} (base {{ ws.branch.base_branch }}) ·
      {{ ws.agent_kind }} · {{ ws.tmux_session }}
      <span v-if="ws.github_repo"> · {{ ws.github_repo }}</span>
    </div>
    <div class="text-xs text-faint font-mono mb-4">
      worktree: {{ ws.work_dir }} ·
      <router-link :to="`/s/${props.id}/files`" class="text-accent hover:underline">browse files →</router-link>
    </div>

    <p v-if="error" class="mb-3 text-sm text-red-400">{{ error }}</p>
    <p v-if="notice" class="mb-3 text-sm text-accent">{{ notice }}</p>

    <section
      v-if="ws.pending_prompt"
      class="mb-5 rounded border border-amber-700/60 bg-amber-950/40 p-4"
    >
      <div class="mb-2 text-xs font-medium text-amber-300">
        Waiting for input — the agent is blocked on this prompt:
      </div>
      <pre
        class="max-h-72 overflow-auto rounded bg-code p-3 text-xs leading-snug text-code-fg whitespace-pre-wrap"
      >{{ ws.pending_prompt }}</pre>
      <p class="mt-2 text-xs text-muted">
        Answer it directly in the terminal below, or <code>loom attach {{ ws.id }}</code>.
      </p>
    </section>

    <div class="grid gap-5 lg:grid-cols-3">
      <!-- Left: goal, description, issues, actions -->
      <div class="space-y-5 lg:col-span-1">
        <section class="rounded border border-line bg-surface p-4">
          <label class="block text-xs text-muted mb-1">Title</label>
          <input
            v-model="titleDraft"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none"
          />
          <button
            class="mt-2 rounded bg-subtle hover:bg-subtle-hover px-2 py-1 text-xs"
            :disabled="busy === 'title'"
            @click="saveTitle"
          >
            Save title
          </button>
        </section>

        <section class="rounded border border-line bg-surface p-4">
          <label class="block text-xs text-muted mb-1">
            Goal — the agent's prompt (may be empty)
          </label>
          <textarea
            v-model="goalDraft"
            rows="3"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none"
          ></textarea>
          <button
            class="mt-2 rounded bg-subtle hover:bg-subtle-hover px-2 py-1 text-xs"
            :disabled="busy === 'goal'"
            @click="saveGoal"
          >
            Save goal
          </button>
        </section>

        <section class="rounded border border-line bg-surface p-4">
          <label class="block text-xs text-muted mb-1">
            Attention — the agent's signal, set via <code>weaver status</code>
          </label>
          <div class="flex gap-2">
            <select
              v-model="attnLevelDraft"
              data-testid="attention-select"
              class="rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            >
              <option value="ok">OK</option>
              <option value="attention">Attention</option>
              <option value="blocked">Blocked</option>
            </select>
            <input
              v-model="attnNoteDraft"
              placeholder="Waiting for PR review feedback"
              class="min-w-0 flex-1 rounded bg-input px-2 py-1.5 text-sm outline-none focus:ring-1 ring-accent"
            />
          </div>
          <button
            class="mt-2 rounded bg-subtle hover:bg-subtle-hover px-2 py-1 text-xs"
            :disabled="busy === 'attention'"
            @click="saveAttention"
          >
            Save attention
          </button>
        </section>

        <section class="rounded border border-line bg-surface p-4">
          <div class="flex items-center justify-between mb-1">
            <label class="text-xs text-muted">Description / current state</label>
            <button
              class="rounded bg-subtle hover:bg-subtle-hover px-2 py-0.5 text-xs"
              :disabled="busy === 'summary'"
              @click="summarize"
            >
              {{ busy === 'summary' ? 'Summarizing…' : 'Summarize now' }}
            </button>
          </div>
          <textarea
            v-model="descDraft"
            rows="6"
            class="w-full rounded bg-input px-2 py-1.5 text-sm outline-none"
          ></textarea>
          <button
            class="mt-2 rounded bg-subtle hover:bg-subtle-hover px-2 py-1 text-xs"
            :disabled="busy === 'desc'"
            @click="saveDesc"
          >
            Save description
          </button>
        </section>

        <section
          v-if="issues.length"
          class="rounded border border-line bg-surface p-4"
          data-testid="issues-panel"
        >
          <div class="flex items-center justify-between mb-2">
            <span class="text-xs text-muted">
              Open issues
              <span class="text-faint">({{ issues.length }})</span>
            </span>
            <span class="text-xs text-faint">read-only · manage with <code>weaver issue</code></span>
          </div>
          <ul class="space-y-2 text-sm">
            <li v-for="i in issues" :key="i.id" class="rounded bg-canvas/60 p-2">
              <div class="flex items-baseline gap-2">
                <span class="font-mono text-xs text-muted">#{{ i.id }}</span>
                <span class="text-fg">{{ i.title }}</span>
              </div>
              <pre
                v-if="i.body"
                class="mt-1 whitespace-pre-wrap text-xs text-muted"
              >{{ i.body }}</pre>
            </li>
          </ul>
        </section>

        <section
          v-if="backlog.length"
          class="rounded border border-line bg-surface p-4"
          data-testid="backlog-panel"
        >
          <div class="flex items-center justify-between mb-2">
            <span class="text-xs text-muted">
              Repo backlog
              <span class="text-faint">({{ backlog.length }})</span>
            </span>
            <span class="text-xs text-faint">unclaimed · whole repo</span>
          </div>
          <ul class="space-y-1 text-sm">
            <li
              v-for="i in backlog"
              :key="i.id"
              class="flex items-baseline gap-2 rounded bg-canvas/60 p-2"
            >
              <span class="font-mono text-xs text-muted">#{{ i.id }}</span>
              <span class="text-fg">{{ i.title }}</span>
            </li>
          </ul>
        </section>

        <ScratchPanel :id="props.id" />

        <section class="rounded border border-line bg-surface p-4 flex gap-2">
          <button
            v-if="ws.status === 'orphaned'"
            class="rounded bg-amber-700 hover:bg-amber-600 px-3 py-1.5 text-sm"
            :disabled="busy === 'adopt'"
            @click="adopt"
          >
            {{ busy === 'adopt' ? 'Adopting…' : 'Adopt' }}
          </button>
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

      <!-- Right: live terminal, events -->
      <div class="space-y-5 lg:col-span-2">
        <section class="rounded border border-line bg-surface p-4">
          <div class="text-xs text-muted mb-2">Terminal</div>
          <AgentTerminal :id="props.id" />
        </section>

        <section class="rounded border border-line bg-surface p-4">
          <div class="text-xs text-muted mb-2">Activity</div>
          <ul class="space-y-1 text-sm max-h-60 overflow-auto">
            <li v-for="ev in events" :key="ev.id" class="flex gap-2">
              <span class="text-faint font-mono text-xs shrink-0">
                {{ ev.created_at.slice(11, 19) }}
              </span>
              <span class="text-muted">{{ eventLine(ev) }}</span>
            </li>
            <li v-if="!events.length" class="text-faint">No activity yet.</li>
          </ul>
        </section>

      </div>
    </div>
  </div>
  <p v-else class="text-muted">Loading…</p>
</template>
