<script setup lang="ts">
import { ref, watch } from 'vue';
import type { Comment, Thread } from '../types';
import { timeAgo } from '../lib/time';

// The presentational margin rail: one card per located open thread (collapsed
// to an author + snippet line; the active one expands to the full thread +
// reply composer), the pending new-thread composer, and a collapsible
// "Unanchored" footer for threads whose quote no longer locates. Purely
// props-in / events-out — ArtifactComments owns all the state and API calls.
const props = defineProps<{
  cards: Array<{ thread: Thread; top: number; active: boolean; located: boolean }>;
  pending: { top: number } | null;
  orphaned: Thread[];
}>();

const emit = defineEmits<{
  reply: [payload: { tid: number; body: string }];
  resolve: [tid: number];
  create: [body: string];
  cancel: [];
  focus: [tid: number];
}>();

const replyDrafts = ref<Record<number, string>>({});
const pendingDraft = ref('');
const showOrphaned = ref(false);

function lastComment(thread: Thread): Comment | undefined {
  return thread.comments[thread.comments.length - 1];
}

function authorLabel(author: string): string {
  return author === 'agent' ? 'Agent' : 'You';
}

function submitReply(tid: number) {
  const body = (replyDrafts.value[tid] ?? '').trim();
  if (!body) return;
  emit('reply', { tid, body });
  // The draft clears only once the reply lands (the thread's comment count
  // grows — see the watch below), so a failed post keeps the text for a retry.
}

function submitCreate() {
  const body = pendingDraft.value.trim();
  if (!body) return;
  emit('create', body);
  // Cleared when the parent closes the composer on success (`pending` → null);
  // a failed create leaves it open with the draft intact.
}

function cancelCreate() {
  pendingDraft.value = '';
  emit('cancel');
}

// Clear the new-thread draft when the parent closes the composer (success only
// — a failed create keeps `pending` non-null, so the text survives).
watch(
  () => props.pending,
  (p) => {
    if (!p) pendingDraft.value = '';
  },
);

// Clear the active card's reply draft when its reply lands — the active
// thread's comment count growing is the success signal (a failed reply never
// grows it, so the draft is preserved).
let activeTid: number | null = null;
let activeCount = -1;
watch(
  () => {
    const a = props.cards.find((c) => c.active);
    return a ? { tid: a.thread.id, n: a.thread.comments.length } : null;
  },
  (now) => {
    if (!now) {
      activeTid = null;
      activeCount = -1;
      return;
    }
    if (now.tid === activeTid && now.n > activeCount) replyDrafts.value[now.tid] = '';
    activeTid = now.tid;
    activeCount = now.n;
  },
  { immediate: true },
);
</script>

<template>
  <div>
    <!-- One card per located open thread, absolutely positioned in a
         right-aligned column; the active one expands in place. -->
    <div
      v-for="card in props.cards"
      :key="card.thread.id"
      class="pointer-events-auto absolute right-2 w-72 rounded border bg-surface p-2 text-xs shadow-sm"
      :class="card.active ? 'border-accent ring-1 ring-accent z-10' : 'border-line hover:border-accent/60'"
      :style="{ top: card.top + 'px' }"
      :data-testid="`comment-card-${card.thread.id}`"
      @click="!card.active && emit('focus', card.thread.id)"
    >
      <template v-if="!card.active">
        <div class="flex items-center gap-1.5">
          <span
            class="inline-flex h-4 items-center rounded-full px-1.5 text-2xs font-medium"
            :class="
              lastComment(card.thread)?.author === 'agent'
                ? 'bg-agent-soft text-agent'
                : 'bg-subtle text-muted'
            "
          >
            {{ authorLabel(lastComment(card.thread)?.author ?? 'user') }}
          </span>
          <span class="text-2xs text-faint">{{ timeAgo(lastComment(card.thread)?.created_at) }}</span>
        </div>
        <p class="mt-1 truncate text-fg">{{ lastComment(card.thread)?.body }}</p>
      </template>

      <template v-else>
        <p class="mb-1.5 truncate italic text-muted" :title="card.thread.anchor.quote">
          &ldquo;{{ card.thread.anchor.quote }}&rdquo;
        </p>
        <div class="max-h-56 space-y-2 overflow-auto pr-0.5">
          <div v-for="c in card.thread.comments" :key="c.seq">
            <div class="flex items-center gap-1.5">
              <span
                class="inline-flex h-4 items-center rounded-full px-1.5 text-2xs font-medium"
                :class="c.author === 'agent' ? 'bg-agent-soft text-agent' : 'bg-subtle text-muted'"
              >
                {{ authorLabel(c.author) }}
              </span>
              <span class="text-2xs text-faint">{{ timeAgo(c.created_at) }}</span>
            </div>
            <p class="mt-0.5 whitespace-pre-wrap text-fg">{{ c.body }}</p>
          </div>
        </div>
        <textarea
          v-model="replyDrafts[card.thread.id]"
          rows="2"
          placeholder="Reply…"
          class="mt-2 w-full resize-none rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
          @click.stop
          @mousedown.stop
        ></textarea>
        <div class="mt-1.5 flex items-center gap-1.5">
          <button
            type="button"
            class="btn-primary px-2 py-1 text-2xs"
            @click.stop="submitReply(card.thread.id)"
          >
            Reply
          </button>
          <button
            type="button"
            class="btn-secondary ml-auto px-2 py-1 text-2xs"
            @click.stop="emit('resolve', card.thread.id)"
          >
            Resolve
          </button>
        </div>
      </template>
    </div>

    <!-- Pending new-thread composer. -->
    <div
      v-if="props.pending"
      class="pointer-events-auto absolute right-2 z-10 w-72 rounded border border-accent bg-surface p-2 text-xs shadow-sm"
      :style="{ top: props.pending.top + 'px' }"
      data-testid="comment-pending"
    >
      <textarea
        v-model="pendingDraft"
        rows="2"
        placeholder="Comment…"
        class="w-full resize-none rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
        @mousedown.stop
      ></textarea>
      <div class="mt-1.5 flex items-center gap-1.5">
        <button type="button" class="btn-primary px-2 py-1 text-2xs" @click="submitCreate">
          Comment
        </button>
        <button type="button" class="btn-secondary px-2 py-1 text-2xs" @click="cancelCreate">
          Cancel
        </button>
      </div>
    </div>

    <!-- Unanchored footer — read-only threads whose quote no longer locates. -->
    <div v-if="props.orphaned.length" class="pointer-events-auto absolute bottom-2 right-2 w-72">
      <button
        type="button"
        class="pill w-full justify-between px-2 py-1 text-2xs"
        @click="showOrphaned = !showOrphaned"
      >
        <span>Unanchored ({{ props.orphaned.length }})</span>
        <span>{{ showOrphaned ? '▾' : '▸' }}</span>
      </button>
      <div
        v-if="showOrphaned"
        class="mt-1 max-h-56 space-y-1.5 overflow-auto rounded border border-line bg-surface p-2 shadow-sm"
      >
        <div
          v-for="t in props.orphaned"
          :key="t.id"
          class="border-b border-line pb-1.5 last:border-0 last:pb-0"
        >
          <p class="truncate italic text-faint">&ldquo;{{ t.anchor.quote }}&rdquo;</p>
          <p class="mt-0.5 truncate text-fg">{{ lastComment(t)?.body }}</p>
          <button
            type="button"
            class="btn-secondary mt-1 px-2 py-0.5 text-2xs"
            @click="emit('resolve', t.id)"
          >
            Resolve
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
