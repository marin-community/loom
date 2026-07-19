<script setup lang="ts">
import { ref, watch } from 'vue';
import type { Comment, Thread } from '../types';
import { timeAgo } from '../lib/time';

// One discussion thread rendered inline in the document flow — right under the
// block its quote anchors to (Google-Wave style, not a margin gutter). Collapsed
// it's a slim one-line chip; the active one expands in place to the full thread
// plus a reply composer. Purely props-in / events-out — ArtifactDocument owns
// the state and the API calls; this only renders and reports intent.
//
// Uses <div> (not <p>) for every text line on purpose: the card renders *inside*
// the rendered `.markdown-body`, whose prose CSS styles `p`/`ul`/`pre` but leaves
// `div`/`button`/`textarea` alone, so nothing bleeds in.
const props = defineProps<{ thread: Thread; active: boolean }>();
const emit = defineEmits<{
  focus: [tid: number];
  reply: [payload: { tid: number; body: string }];
  resolve: [tid: number];
}>();

const draft = ref('');

function lastComment(t: Thread): Comment | undefined {
  return t.comments[t.comments.length - 1];
}
function authorLabel(author: string): string {
  return author === 'agent' ? 'Agent' : 'You';
}

function submitReply() {
  const body = draft.value.trim();
  if (!body) return;
  emit('reply', { tid: props.thread.id, body });
  // The draft clears only once the reply lands — the thread's comment count
  // growing is the success signal (see the watch below) — so a failed post
  // keeps the text for a retry.
}

let count = props.thread.comments.length;
watch(
  () => props.thread.comments.length,
  (n) => {
    if (n > count) draft.value = '';
    count = n;
  },
);
</script>

<template>
  <!-- Collapsed: a slim clickable chip that keeps the document quiet. -->
  <button
    v-if="!active"
    type="button"
    class="my-1.5 flex w-full items-center gap-1.5 rounded border border-line bg-subtle/50 px-2 py-1 text-left text-xs hover:border-accent/60"
    :data-testid="`comment-card-${thread.id}`"
    @click.stop="emit('focus', thread.id)"
  >
    <span aria-hidden="true">💬</span>
    <span
      class="inline-flex h-4 shrink-0 items-center rounded-full px-1.5 text-2xs font-medium"
      :class="
        lastComment(thread)?.author === 'agent'
          ? 'bg-agent-soft text-agent'
          : 'bg-surface text-muted'
      "
    >
      {{ authorLabel(lastComment(thread)?.author ?? 'user') }}
    </span>
    <span class="min-w-0 flex-1 truncate text-fg">{{ lastComment(thread)?.body }}</span>
    <span v-if="thread.comments.length > 1" class="shrink-0 text-2xs text-faint">{{
      thread.comments.length
    }}</span>
  </button>

  <!-- Expanded: the full thread and reply composer, in place. -->
  <div
    v-else
    class="my-2 rounded border border-accent bg-subtle/40 p-2 text-xs ring-1 ring-accent"
    :data-testid="`comment-card-${thread.id}`"
    @click.stop
  >
    <div class="mb-1.5 truncate italic text-muted" :title="thread.anchor.quote">
      &ldquo;{{ thread.anchor.quote }}&rdquo;
    </div>
    <div class="space-y-2">
      <div v-for="c in thread.comments" :key="c.seq">
        <div class="flex items-center gap-1.5">
          <span
            class="inline-flex h-4 items-center rounded-full px-1.5 text-2xs font-medium"
            :class="c.author === 'agent' ? 'bg-agent-soft text-agent' : 'bg-surface text-muted'"
          >
            {{ authorLabel(c.author) }}
          </span>
          <span class="text-2xs text-faint">{{ timeAgo(c.created_at) }}</span>
        </div>
        <div class="mt-0.5 whitespace-pre-wrap text-fg">{{ c.body }}</div>
      </div>
    </div>
    <textarea
      v-model="draft"
      rows="2"
      placeholder="Reply…"
      class="mt-2 w-full resize-none rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
      @click.stop
      @mousedown.stop
    ></textarea>
    <div class="mt-1.5 flex items-center gap-1.5">
      <button type="button" class="btn-primary px-2 py-1 text-2xs" @click.stop="submitReply">
        Reply
      </button>
      <button
        type="button"
        class="btn-secondary ml-auto px-2 py-1 text-2xs"
        @click.stop="emit('resolve', thread.id)"
      >
        Resolve
      </button>
    </div>
  </div>
</template>
