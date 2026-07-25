<script setup lang="ts">
import { nextTick, ref, watch } from 'vue';
import type { Review, ReviewComment } from '../types';

const props = defineProps<{
  review: Review;
  comment: ReviewComment;
  active: boolean;
  reanchoring: boolean;
  error: string;
}>();
const emit = defineEmits<{
  focus: [commentId: number];
  close: [commentId: number];
  edit: [payload: { commentId: number; body: string }];
  delete: [commentId: number];
  reanchor: [commentId: number];
  cancelReanchor: [];
  resolution: [commentId: number, resolved: boolean];
}>();

const body = ref(props.comment.body);
const editing = ref(false);
const cardEl = ref<HTMLElement | null>(null);
const editEl = ref<HTMLButtonElement | null>(null);
const textareaEl = ref<HTMLTextAreaElement | null>(null);

watch(
  () => props.comment.body,
  (next) => {
    body.value = next;
  },
);

watch(
  () => props.active,
  (active) => {
    if (active) void nextTick(() => cardEl.value?.focus());
  },
);

function beginEdit() {
  editing.value = true;
  void nextTick(() => textareaEl.value?.focus());
}

function save() {
  const next = body.value.trim();
  if (!next) return;
  emit('edit', { commentId: props.comment.id, body: next });
  editing.value = false;
  void nextTick(() => editEl.value?.focus());
}

function cancelEdit() {
  body.value = props.comment.body;
  editing.value = false;
  void nextTick(() => editEl.value?.focus());
}
</script>

<template>
  <button
    v-if="!active"
    type="button"
    class="my-1.5 flex w-full items-center gap-2 rounded border px-2 py-1 text-left text-xs"
    :class="
      review.status === 'draft'
        ? 'border-accent/60 bg-subtle text-fg hover:border-accent'
        : 'border-line bg-subtle/50 text-muted hover:border-accent/60'
    "
    :data-testid="`review-comment-${comment.id}`"
    :data-review-collapsed="comment.id"
    @click.stop="emit('focus', comment.id)"
  >
    <span
      class="inline-flex shrink-0 items-center rounded-full px-1.5 py-0.5 text-2xs font-semibold uppercase tracking-wide"
      :class="review.status === 'draft' ? 'bg-accent text-accent-fg' : 'bg-surface text-faint'"
    >
      {{
        review.status === 'draft'
          ? 'Pending'
          : comment.status === 'resolved'
            ? 'Resolved'
            : review.legacy
              ? 'Earlier thread'
              : 'Submitted'
      }}
    </span>
    <span class="min-w-0 flex-1 truncate">{{ comment.body }}</span>
    <span v-if="review.outdated" class="shrink-0 text-2xs text-block">Stale</span>
  </button>

  <div
    v-else
    ref="cardEl"
    tabindex="-1"
    class="my-2 rounded border p-2 text-xs ring-1"
    :class="
      review.status === 'draft'
        ? 'border-accent bg-subtle/40 ring-accent/40'
        : 'border-accent bg-subtle/40 ring-accent/30'
    "
    :data-testid="`review-comment-${comment.id}`"
    :data-review-card="comment.id"
    @click.stop
    @keydown.esc.stop.prevent="emit('close', comment.id)"
  >
    <div class="mb-1.5 flex items-start gap-2">
      <div class="min-w-0 flex-1 truncate italic text-muted" :title="comment.anchor.quote">
        &ldquo;{{ comment.anchor.quote }}&rdquo;
      </div>
      <span
        v-if="review.outdated || comment.subject_version !== review.subject.current_version"
        class="shrink-0 rounded bg-block-soft px-1.5 py-0.5 text-2xs font-medium text-block"
      >
        Revision {{ comment.subject_version }} · stale
      </span>
      <span
        v-else-if="comment.status === 'resolved'"
        class="shrink-0 rounded bg-subtle px-1.5 py-0.5 text-2xs font-medium text-muted"
      >
        Resolved
      </span>
    </div>

    <template v-if="review.legacy">
      <div
        v-for="legacyComment in review.comments"
        :key="legacyComment.id"
        class="mt-1 whitespace-pre-wrap text-fg"
      >
        {{ legacyComment.body }}
      </div>
    </template>
    <template v-else-if="review.status === 'draft'">
      <textarea
        v-if="editing"
        ref="textareaEl"
        v-model="body"
        rows="3"
        class="w-full resize-y rounded border border-line bg-input p-1.5 text-xs text-fg outline-none focus:border-accent"
        data-testid="review-comment-edit"
        @keydown.ctrl.enter.prevent="save"
        @keydown.meta.enter.prevent="save"
        @keydown.esc.stop.prevent="cancelEdit"
      ></textarea>
      <div v-else class="whitespace-pre-wrap text-fg">{{ comment.body }}</div>
    </template>
    <div v-else class="whitespace-pre-wrap text-fg">{{ comment.body }}</div>

    <div class="mt-2 flex flex-wrap items-center gap-1.5">
      <template v-if="review.status === 'draft'">
        <button
          v-if="editing"
          type="button"
          class="btn-primary px-2 py-1 text-2xs"
          @click.stop="save"
        >
          Save
        </button>
        <button
          v-if="editing"
          type="button"
          class="btn-secondary px-2 py-1 text-2xs"
          @click.stop="cancelEdit"
        >
          Cancel
        </button>
        <button
          v-if="!editing"
          ref="editEl"
          type="button"
          class="btn-secondary px-2 py-1 text-2xs"
          @click.stop="beginEdit"
        >
          Edit
        </button>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-2xs"
          @click.stop="reanchoring ? emit('cancelReanchor') : emit('reanchor', comment.id)"
        >
          {{ reanchoring ? 'Cancel re-anchor' : 'Re-anchor' }}
        </button>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-2xs text-block"
          @click.stop="emit('delete', comment.id)"
        >
          Delete
        </button>
      </template>
      <button
        v-if="!review.legacy && review.status === 'submitted'"
        type="button"
        class="btn-secondary px-2 py-1 text-2xs"
        @click.stop="emit('resolution', comment.id, comment.status !== 'resolved')"
      >
        {{ comment.status === 'resolved' ? 'Reopen' : 'Resolve' }}
      </button>
      <button
        v-if="review.legacy && comment.status === 'open'"
        type="button"
        class="btn-secondary px-2 py-1 text-2xs"
        @click.stop="emit('resolution', comment.id, true)"
      >
        Resolve
      </button>
      <button
        type="button"
        class="btn-secondary ml-auto px-2 py-1 text-2xs"
        @click.stop="emit('close', comment.id)"
      >
        Close
      </button>
    </div>
    <p v-if="error" class="mt-2 rounded bg-block-soft px-2 py-1 text-2xs text-block" role="alert">
      {{ error }}
    </p>
    <p v-if="reanchoring" class="mt-2 rounded bg-subtle px-2 py-1 text-2xs text-accent">
      Select the replacement text in this artifact, then choose Re-anchor selection.
    </p>
  </div>
</template>
