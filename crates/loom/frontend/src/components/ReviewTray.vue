<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import type { Review } from '../types';
import InlineConfirm from './InlineConfirm.vue';

const props = defineProps<{
  reviews: Review[];
  draft: Review | null;
  open: boolean;
  overallNote: string;
  summarySaving: boolean;
  acknowledgeOutdated: boolean;
  error: string;
  layoutBusy: boolean;
  submitting: boolean;
  discarding: boolean;
  deliveryErrors: Record<number, string>;
  subjectLabel: string;
  discardAction: () => Promise<void>;
}>();
const emit = defineEmits<{
  'update:open': [value: boolean];
  'update:overallNote': [value: string];
  'update:acknowledgeOutdated': [value: boolean];
  navigate: [direction: number];
  focusComment: [commentId: number];
  saveOverall: [];
  retarget: [];
  submit: [];
  retry: [review: Review];
}>();
const toggleEl = ref<HTMLButtonElement | null>(null);
const overallId = `review-overall-${useId()}`;
defineExpose({ focusToggle: () => toggleEl.value?.focus() });

const failed = computed(() =>
  props.reviews.filter(
    (review) =>
      review.status === 'submitted' && !review.legacy && review.delivery_state === 'failed',
  ),
);
const recent = computed(
  () =>
    [...props.reviews]
      .filter((review) => review.status === 'submitted' && !review.legacy)
      .sort((a, b) => b.id - a.id)[0] ?? null,
);
</script>

<template>
  <aside
    class="absolute bottom-3 right-3 z-20 w-[min(28rem,calc(100%-1.5rem))] rounded-lg border border-line bg-surface shadow-xl"
    data-testid="review-tray"
    aria-label="Review tray"
  >
    <div class="flex min-h-10 items-center gap-1.5 px-2">
      <button
        ref="toggleEl"
        type="button"
        class="min-w-0 flex-1 px-1 py-2 text-left text-xs font-semibold text-fg"
        data-testid="review-tray-toggle"
        :aria-expanded="open"
        @click="emit('update:open', !open)"
      >
        <template v-if="draft">
          Review · {{ draft.comments.length }} pending
          <span v-if="draft.outdated" class="ml-1 text-block">· stale</span>
          <span v-if="failed.length" class="ml-1 text-block">
            · {{ failed.length }} delivery failed
          </span>
        </template>
        <template v-else-if="failed.length">Review delivery · {{ failed.length }} failed</template>
        <template v-else-if="recent">Review submitted · {{ recent.delivery_state }}</template>
        <template v-else>Review {{ subjectLabel }}</template>
      </button>
      <template v-if="draft?.comments.length">
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          aria-label="Previous pending comment"
          @click="emit('navigate', -1)"
        >
          ↑
        </button>
        <button
          type="button"
          class="btn-secondary px-2 py-1 text-xs"
          aria-label="Next pending comment"
          @click="emit('navigate', 1)"
        >
          ↓
        </button>
      </template>
      <button
        type="button"
        class="btn-secondary px-2 py-1 text-xs"
        :aria-label="open ? 'Collapse review tray' : 'Expand review tray'"
        @click="emit('update:open', !open)"
      >
        {{ open ? '▾' : '▴' }}
      </button>
    </div>

    <div v-if="open" class="max-h-[min(60vh,34rem)] overflow-auto border-t border-line p-3">
      <div v-if="failed.length" class="mb-3 space-y-2" aria-label="Failed review deliveries">
        <div
          v-for="item in failed"
          :key="item.id"
          class="rounded border border-block-line bg-block-soft p-2 text-xs"
          :data-testid="`failed-review-delivery-${item.id}`"
        >
          <p class="font-medium text-block">Review {{ item.id }} delivery failed</p>
          <p v-if="item.delivery_error" class="mt-1 text-block">{{ item.delivery_error }}</p>
          <button
            type="button"
            class="btn-primary mt-2 px-2 py-1 text-xs"
            @click="emit('retry', item)"
          >
            Retry delivery
          </button>
          <p
            v-if="deliveryErrors[item.id]"
            class="mt-2 rounded bg-surface/70 px-2 py-1 text-2xs text-block"
            role="alert"
          >
            {{ deliveryErrors[item.id] }}
          </p>
        </div>
      </div>

      <template v-if="draft">
        <p
          v-if="draft.outdated"
          class="mb-3 rounded border border-block-line bg-block-soft p-2 text-xs text-block"
          data-testid="review-stale-warning"
        >
          This {{ subjectLabel }} changed to {{ draft.subject.current_version }}. Captured anchors
          are preserved; re-anchor them, or acknowledge the older context before submitting.
        </p>
        <button
          v-if="draft.outdated && !draft.comments.length"
          type="button"
          class="btn-secondary mb-3 px-2 py-1 text-xs"
          data-testid="review-retarget-current"
          :disabled="summarySaving"
          @click="emit('retarget')"
        >
          Move review to current {{ subjectLabel }}
        </button>

        <div class="space-y-1.5" aria-label="Pending comments">
          <button
            v-for="(comment, index) in draft.comments"
            :key="comment.id"
            type="button"
            class="flex w-full items-center gap-2 rounded border border-line px-2 py-1.5 text-left text-xs hover:border-accent"
            @click="emit('focusComment', comment.id)"
          >
            <span class="shrink-0 font-mono text-2xs text-faint">{{ index + 1 }}</span>
            <span class="min-w-0 flex-1 truncate text-fg">{{ comment.body }}</span>
            <span
              v-if="comment.subject_version !== draft.subject.current_version"
              class="shrink-0 text-2xs text-block"
            >
              stale
            </span>
          </button>
        </div>

        <label class="mt-3 block text-xs font-medium text-muted" :for="overallId">
          Overall note <span class="font-normal text-faint">(optional)</span>
        </label>
        <textarea
          :id="overallId"
          :value="overallNote"
          rows="3"
          class="mt-1 w-full resize-y rounded border border-line bg-input p-2 text-xs text-fg outline-none focus:border-accent"
          :placeholder="`Feedback that applies to the ${subjectLabel} as a whole…`"
          data-testid="review-overall-note"
          :disabled="layoutBusy || submitting || discarding"
          @input="emit('update:overallNote', ($event.target as HTMLTextAreaElement).value)"
          @blur="emit('saveOverall')"
        ></textarea>
        <p v-if="summarySaving" class="mt-1 text-2xs text-faint" aria-live="polite">
          Saving overall note…
        </p>

        <label
          v-if="draft.outdated"
          class="mt-2 flex cursor-pointer items-start gap-2 rounded bg-subtle/50 p-2 text-xs text-muted"
        >
          <input
            :checked="acknowledgeOutdated"
            type="checkbox"
            class="mt-0.5"
            data-testid="review-stale-ack"
            @change="
              emit('update:acknowledgeOutdated', ($event.target as HTMLInputElement).checked)
            "
          />
          Submit against the captured version intentionally.
        </label>

        <div class="mt-3 border-t border-line pt-3">
          <p class="mb-2 text-2xs font-semibold uppercase tracking-wide text-faint">
            Conversation feedback preview
          </p>
          <pre
            class="max-h-32 overflow-auto whitespace-pre-wrap rounded bg-code p-2 text-xs text-code-fg"
            >{{ draft.message }}</pre>
        </div>

        <div class="mt-3 flex flex-wrap items-center gap-2">
          <p
            v-if="error"
            class="w-full rounded bg-block-soft px-2 py-1 text-2xs text-block"
            role="alert"
          >
            {{ error }}
          </p>
          <InlineConfirm
            label="Discard draft"
            :message="`Discard the overall note and all ${draft.comments.length} pending comments?`"
            confirm-label="Discard draft"
            danger
            :disabled="layoutBusy || submitting || discarding"
            :action="discardAction"
          />
          <button
            type="button"
            class="btn-primary ml-auto px-3 py-1.5 text-xs"
            data-testid="submit-review"
            :disabled="
              submitting ||
              layoutBusy ||
              discarding ||
              summarySaving ||
              (!draft.comments.length && !overallNote.trim()) ||
              (draft.outdated && !acknowledgeOutdated)
            "
            @click="emit('submit')"
          >
            {{ submitting ? 'Submitting…' : 'Submit review' }}
          </button>
        </div>
      </template>

      <template v-else>
        <div
          v-if="recent && !failed.some((review) => review.id === recent?.id)"
          class="mb-3 text-xs text-muted"
        >
          <p class="font-medium text-fg">Review {{ recent.id }} submitted</p>
          <p class="mt-1">
            Delivery: <span class="text-accent">{{ recent.delivery_state }}</span>
          </p>
        </div>
        <label class="block text-xs font-medium text-muted" :for="overallId">
          Start a new review with an overall note
        </label>
        <textarea
          :id="overallId"
          :value="overallNote"
          rows="3"
          class="mt-1 w-full resize-y rounded border border-line bg-input p-2 text-xs text-fg outline-none focus:border-accent"
          :placeholder="`Feedback that applies to the ${subjectLabel} as a whole…`"
          data-testid="review-overall-note"
          :disabled="layoutBusy || submitting || discarding"
          @input="emit('update:overallNote', ($event.target as HTMLTextAreaElement).value)"
          @blur="emit('saveOverall')"
        ></textarea>
        <p v-if="summarySaving" class="mt-1 text-2xs text-faint" aria-live="polite">
          Saving overall note…
        </p>
        <p
          v-if="error"
          class="mt-2 rounded bg-block-soft px-2 py-1 text-2xs text-block"
          role="alert"
        >
          {{ error }}
        </p>
      </template>
    </div>
  </aside>
</template>
