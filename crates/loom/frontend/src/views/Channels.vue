<script setup lang="ts">
import {
  computed,
  nextTick,
  onActivated,
  onDeactivated,
  onMounted,
  onUnmounted,
  ref,
  watch,
} from 'vue';
import { useRouter } from 'vue-router';
import {
  createChannel,
  listChannels,
  listChannelMessages,
  markChannelRead,
  sendChannelMessage,
} from '../api';
import type { Channel, ChannelMessage } from '../types';
import { timeAgo } from '../lib/time';
import { useCommandScope, type Command } from '../lib/commands';

defineOptions({ name: 'Channels' });
const props = defineProps<{ id?: string }>();
const router = useRouter();

const channels = ref<Channel[]>([]);
const messages = ref<ChannelMessage[]>([]);
const selectedId = ref(props.id || '');
const query = ref('');
const loaded = ref(false);
const loadingMessages = ref(false);
const sending = ref(false);
const error = ref('');
const draft = ref('');
const createOpen = ref(false);
const createName = ref('');
const createTopic = ref('');
const createRepo = ref('');
const creating = ref(false);
const composer = ref<HTMLTextAreaElement | null>(null);
const createNameInput = ref<HTMLInputElement | null>(null);
let timer: number | undefined;
let active = true;

const selected = computed(
  () => channels.value.find((channel) => channel.id === selectedId.value) ?? null,
);
const visible = computed(() => {
  const needle = query.value.trim().toLowerCase();
  if (!needle) return channels.value;
  return channels.value.filter((channel) =>
    [channel.name, channel.topic, channel.id, channel.repo_root]
      .join(' ')
      .toLowerCase()
      .includes(needle),
  );
});
const urgentTotal = computed(() =>
  channels.value.reduce((count, channel) => count + channel.unread_urgent_count, 0),
);
const repos = computed(() =>
  [...new Set(channels.value.map((channel) => channel.repo_root))].sort(),
);

async function loadChannels(selectFallback = true) {
  try {
    channels.value = await listChannels();
    if (
      selectFallback &&
      (!selectedId.value || !channels.value.some((channel) => channel.id === selectedId.value))
    ) {
      selectedId.value = channels.value[0]?.id || '';
      if (selectedId.value) await router.replace(`/channels/${selectedId.value}`);
    }
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    loaded.value = true;
  }
}

async function loadMessages(markRead = true) {
  const id = selectedId.value;
  if (!id) {
    messages.value = [];
    return;
  }
  loadingMessages.value = true;
  try {
    const next = await listChannelMessages(id);
    if (id !== selectedId.value) return;
    messages.value = next;
    if (markRead && active && next.length) {
      await markChannelRead(id, next.at(-1)?.seq);
      const row = channels.value.find((channel) => channel.id === id);
      if (row) {
        row.unread_count = 0;
        row.unread_urgent_count = 0;
      }
    }
    error.value = '';
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    loadingMessages.value = false;
  }
}

async function selectChannel(channel: Channel) {
  if (channel.id === selectedId.value) return;
  selectedId.value = channel.id;
  await router.push(`/channels/${channel.id}`);
  await loadMessages();
  await focusSelected();
}

async function focusSelected() {
  await nextTick();
  document
    .querySelector<HTMLElement>(`[data-channel-id="${CSS.escape(selectedId.value)}"]`)
    ?.focus({ preventScroll: true });
}

async function moveSelection(direction: -1 | 1) {
  if (!visible.value.length) return;
  const current = visible.value.findIndex((channel) => channel.id === selectedId.value);
  const index =
    current < 0
      ? direction > 0
        ? 0
        : visible.value.length - 1
      : Math.min(Math.max(current + direction, 0), visible.value.length - 1);
  await selectChannel(visible.value[index]);
}

async function moveEdge(edge: 'first' | 'last') {
  const channel = edge === 'first' ? visible.value[0] : visible.value.at(-1);
  if (channel) await selectChannel(channel);
}

async function send() {
  const body = draft.value.trim();
  if (!body || !selected.value || sending.value) return;
  sending.value = true;
  try {
    const message = await sendChannelMessage(selected.value.id, body);
    messages.value.push(message);
    draft.value = '';
    await loadChannels(false);
    await nextTick();
    composer.value?.focus();
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    sending.value = false;
  }
}

async function openCreate() {
  createOpen.value = true;
  createRepo.value ||= selected.value?.repo_root || repos.value[0] || '';
  await nextTick();
  createNameInput.value?.focus();
}

function cancelCreate() {
  createOpen.value = false;
  createName.value = '';
  createTopic.value = '';
}

async function submitCreate() {
  if (!createName.value.trim() || !createRepo.value.trim() || creating.value) return;
  creating.value = true;
  try {
    const channel = await createChannel(
      createName.value.trim(),
      createTopic.value.trim(),
      createRepo.value.trim(),
    );
    channels.value.unshift(channel);
    cancelCreate();
    await selectChannel(channel);
  } catch (cause) {
    error.value = (cause as Error).message;
  } finally {
    creating.value = false;
  }
}

async function refresh() {
  await Promise.all([loadChannels(false), loadMessages()]);
}

function startPolling() {
  active = true;
  window.clearInterval(timer);
  timer = window.setInterval(() => void refresh(), 3000);
}

function stopPolling() {
  active = false;
  window.clearInterval(timer);
  timer = undefined;
}

const commands = computed<Command[]>(() => [
  { id: 'channels.down', label: 'Next channel', keys: ['j'], run: () => moveSelection(1) },
  { id: 'channels.up', label: 'Previous channel', keys: ['k'], run: () => moveSelection(-1) },
  { id: 'channels.first', label: 'First channel', keys: ['g g'], run: () => moveEdge('first') },
  { id: 'channels.last', label: 'Last channel', keys: ['G'], run: () => moveEdge('last') },
  {
    id: 'channels.compose',
    label: 'Write message',
    keys: ['c'],
    run: () => composer.value?.focus(),
  },
  { id: 'channels.new', label: 'New channel', keys: ['n'], run: openCreate },
  { id: 'channels.refresh', label: 'Refresh channels', keys: ['r'], run: refresh },
]);
useCommandScope('channels', 'Channels', commands, 20);

watch(
  () => props.id,
  async (id) => {
    if (!id || id === selectedId.value) return;
    selectedId.value = id;
    await loadMessages();
  },
);

onMounted(async () => {
  await loadChannels();
  await loadMessages();
  startPolling();
});
onActivated(() => {
  startPolling();
  void refresh();
});
onDeactivated(stopPolling);
onUnmounted(stopPolling);

function repoName(path: string): string {
  return path.replace(/\/+$/, '').split('/').pop() || path;
}

function deliveryLabel(message: ChannelMessage): string {
  if (!message.deliveries.length) return '';
  const failed = message.deliveries.filter((delivery) => delivery.state === 'failed');
  if (failed.length === 1 && message.deliveries.length === 1) {
    return `delivery failed: ${failed[0].last_error}`;
  }
  if (failed.length) return `${failed.length}/${message.deliveries.length} deliveries failed`;
  if (message.deliveries.length === 1) return message.deliveries[0].state;
  const delivered = message.deliveries.filter((delivery) => delivery.state === 'delivered').length;
  return `${delivered}/${message.deliveries.length} delivered`;
}

function deliveryFailed(message: ChannelMessage): boolean {
  return message.deliveries.some((delivery) => delivery.state === 'failed');
}
</script>

<template>
  <section class="flex min-h-0 flex-1 font-mono" data-testid="channels-view">
    <aside class="flex w-80 shrink-0 flex-col border-r border-line bg-surface">
      <header class="flex h-10 items-center border-b border-line px-3">
        <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Channels</h1>
        <span v-if="urgentTotal" class="ml-2 text-2xs text-attn-line">!{{ urgentTotal }}</span>
        <span class="ml-auto text-2xs text-faint">{{ channels.length }} open</span>
        <button
          type="button"
          class="ml-2 border border-line px-1.5 py-0.5 text-2xs text-muted hover:border-accent hover:text-fg"
          title="New channel (n)"
          data-testid="new-channel"
          @click="openCreate"
        >
          +
        </button>
      </header>
      <div class="border-b border-line p-2">
        <input
          v-model="query"
          type="search"
          placeholder="/ filter channels"
          class="w-full bg-input px-2 py-1 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
          @keydown.esc="($event.currentTarget as HTMLInputElement).blur()"
        />
      </div>
      <form
        v-if="createOpen"
        class="space-y-2 border-b border-line bg-canvas p-2"
        data-testid="channel-create-form"
        @submit.prevent="submitCreate"
      >
        <div class="flex items-center gap-2 text-2xs">
          <span class="text-accent">NEW</span>
          <span class="text-faint">explicit agent pipe</span>
          <button
            type="button"
            class="ml-auto text-faint hover:text-fg"
            aria-label="Cancel new channel"
            @click="cancelCreate"
          >
            esc
          </button>
        </div>
        <input
          ref="createNameInput"
          v-model="createName"
          required
          maxlength="120"
          placeholder="channel name"
          class="w-full bg-input px-2 py-1 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
          data-testid="channel-create-name"
          @keydown.esc.prevent="cancelCreate"
        />
        <input
          v-model="createTopic"
          maxlength="4096"
          placeholder="topic (optional)"
          class="w-full bg-input px-2 py-1 text-xs text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
          data-testid="channel-create-topic"
          @keydown.esc.prevent="cancelCreate"
        />
        <select
          v-model="createRepo"
          required
          class="w-full bg-input px-2 py-1 text-xs text-muted outline-none ring-accent focus:ring-1"
          data-testid="channel-create-repo"
          @keydown.esc.prevent="cancelCreate"
        >
          <option value="" disabled>repository</option>
          <option v-for="repo in repos" :key="repo" :value="repo">{{ repoName(repo) }}</option>
        </select>
        <button
          type="submit"
          :disabled="creating || !createName.trim() || !createRepo"
          class="w-full border border-accent px-2 py-1 text-2xs text-accent disabled:opacity-40"
        >
          {{ creating ? 'opening…' : 'open channel' }}
        </button>
      </form>
      <div class="min-h-0 flex-1 overflow-y-auto">
        <button
          v-for="channel in visible"
          :key="channel.id"
          type="button"
          :data-channel-id="channel.id"
          class="block w-full border-b border-line px-3 py-2 text-left outline-none hover:bg-subtle focus:bg-subtle"
          :class="channel.id === selectedId ? 'bg-subtle text-fg' : 'text-muted'"
          @click="selectChannel(channel)"
        >
          <span class="flex min-w-0 items-baseline gap-2">
            <span
              class="w-3 shrink-0 text-center"
              :class="channel.unread_urgent_count ? 'text-attn-line' : 'text-faint'"
            >
              {{ channel.unread_urgent_count ? '!' : channel.unread_count ? '•' : ' ' }}
            </span>
            <span class="min-w-0 flex-1 truncate text-xs font-medium">{{ channel.name }}</span>
            <span v-if="channel.unread_count" class="text-2xs text-accent">
              +{{ channel.unread_count }}
            </span>
          </span>
          <span class="mt-0.5 flex min-w-0 items-center gap-2 pl-5 text-2xs text-faint">
            <span>{{ channel.kind === 'session' ? 'sess' : 'chan' }}</span>
            <span class="truncate">{{ repoName(channel.repo_root) }}</span>
            <span class="ml-auto shrink-0">
              {{ channel.last_message ? timeAgo(channel.last_message.created_at) : '' }}
            </span>
          </span>
          <span v-if="channel.last_message" class="mt-1 block truncate pl-5 text-2xs text-faint">
            {{ channel.last_message.body }}
          </span>
        </button>
        <p v-if="loaded && !visible.length" class="p-4 text-center text-xs text-faint">
          no matching channels
        </p>
      </div>
      <footer class="border-t border-line px-3 py-1.5 text-2xs text-faint">
        j/k move · n new · c compose · r refresh
      </footer>
    </aside>

    <main v-if="selected" class="flex min-w-0 flex-1 flex-col bg-canvas">
      <header class="flex min-h-10 items-center gap-3 border-b border-line px-4 py-1.5">
        <span class="text-xs text-accent">#</span>
        <div class="min-w-0">
          <h2 class="truncate text-sm font-medium text-fg">{{ selected.name }}</h2>
          <p class="truncate text-2xs text-faint">{{ selected.topic || selected.id }}</p>
        </div>
        <router-link
          v-if="selected.session_id"
          :to="`/s/${selected.session_id}`"
          class="ml-auto shrink-0 border border-line px-2 py-1 text-2xs text-muted hover:border-accent hover:text-fg"
        >
          open session ↗
        </router-link>
      </header>

      <p v-if="error" class="border-b border-line px-4 py-2 text-xs text-block">{{ error }}</p>
      <div class="min-h-0 flex-1 overflow-y-auto" data-testid="channel-messages">
        <article
          v-for="message in messages"
          :key="message.id"
          class="grid grid-cols-[3rem_5rem_minmax(0,1fr)] gap-2 border-b border-line px-4 py-2 text-xs"
          :class="
            message.urgency === 'blocked'
              ? 'border-l-2 border-l-block-line'
              : message.urgency === 'attention'
                ? 'border-l-2 border-l-attn-line'
                : ''
          "
        >
          <span class="text-right text-faint">{{ message.seq }}</span>
          <span
            class="uppercase"
            :class="
              message.urgency === 'blocked'
                ? 'text-block'
                : message.urgency === 'attention'
                  ? 'text-attn-line'
                  : 'text-muted'
            "
          >
            {{ message.kind }}
          </span>
          <div class="min-w-0">
            <div class="mb-1 flex items-center gap-2 text-2xs text-faint">
              <span>{{ message.author_kind }}:{{ message.author_id }}</span>
              <span>{{ timeAgo(message.created_at) }}</span>
              <span
                v-if="deliveryLabel(message)"
                :class="deliveryFailed(message) ? 'text-block' : 'text-ok'"
              >
                {{ deliveryLabel(message) }}
              </span>
            </div>
            <p class="whitespace-pre-wrap break-words font-sans text-sm leading-relaxed text-fg">
              {{ message.body }}
            </p>
          </div>
        </article>
        <p v-if="loadingMessages && !messages.length" class="p-6 text-center text-xs text-faint">
          loading channel…
        </p>
        <p
          v-else-if="!loadingMessages && !messages.length"
          class="p-6 text-center text-xs text-faint"
        >
          channel is quiet
        </p>
      </div>

      <form class="border-t border-line bg-surface p-3" @submit.prevent="send">
        <div class="flex items-end gap-2">
          <span class="pb-2 text-xs text-accent">&gt;</span>
          <textarea
            ref="composer"
            v-model="draft"
            rows="2"
            :disabled="sending || selected.state !== 'open'"
            placeholder="message this channel…  Ctrl+Enter sends · Esc leaves input"
            class="min-h-12 min-w-0 flex-1 resize-none bg-input px-3 py-2 font-sans text-sm text-fg outline-none ring-accent placeholder:text-faint focus:ring-1"
            @keydown.ctrl.enter.prevent="send"
            @keydown.meta.enter.prevent="send"
            @keydown.esc="composer?.blur()"
          ></textarea>
          <button
            type="submit"
            :disabled="sending || !draft.trim()"
            class="border border-accent px-3 py-2 text-xs text-accent disabled:opacity-40"
          >
            {{ sending ? 'sending…' : 'send' }}
          </button>
        </div>
      </form>
    </main>
    <main v-else class="grid min-w-0 flex-1 place-items-center text-xs text-faint">
      select a channel
    </main>
  </section>
</template>
