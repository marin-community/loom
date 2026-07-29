<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from 'vue';
import type { ToolContent } from '../types';

const props = defineProps<{
  content: ToolContent;
  title: string;
}>();

interface DiffLine {
  sign: '-' | '+';
  text: string;
}

function diffLines(content: Extract<ToolContent, { type: 'diff' }>): DiffLine[] {
  const lines: DiffLine[] = [];
  const push = (sign: '-' | '+', body: string) => {
    for (const line of body.replace(/\n$/, '').split('\n')) lines.push({ sign, text: line });
  };
  if (content.old) push('-', content.old);
  push('+', content.new);
  return lines;
}

const imageSrc = computed(() => {
  if (props.content.type !== 'image') return '';
  if (!/^image\/[a-z0-9.+-]+$/i.test(props.content.mime_type)) return '';
  return `data:${props.content.mime_type};base64,${props.content.data}`;
});
const detailLabel = computed(() => {
  if (props.content.type === 'diff') return props.content.path;
  if (props.content.type === 'image') return props.content.uri ?? props.content.mime_type;
  return 'Command output';
});

const expanded = ref(false);
const closeButton = ref<HTMLButtonElement | null>(null);
const titleId = `${useId()}-title`;
let returnFocus: HTMLElement | null = null;
let previousBodyOverflow = '';

function openViewer() {
  returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  expanded.value = true;
}

function closeViewer() {
  expanded.value = false;
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (!expanded.value) return;
  if (event.key === 'Escape') {
    event.preventDefault();
    closeViewer();
  } else if (event.key === 'Tab') {
    event.preventDefault();
    closeButton.value?.focus();
  }
}

watch(expanded, async (open) => {
  if (open) {
    previousBodyOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    document.addEventListener('keydown', onDocumentKeydown);
    await nextTick();
    closeButton.value?.focus();
  } else {
    document.body.style.overflow = previousBodyOverflow;
    document.removeEventListener('keydown', onDocumentKeydown);
    await nextTick();
    returnFocus?.focus();
    returnFocus = null;
  }
});

onBeforeUnmount(() => {
  if (expanded.value) document.body.style.overflow = previousBodyOverflow;
  document.removeEventListener('keydown', onDocumentKeydown);
});
</script>

<template>
  <div class="tool-preview">
    <button
      v-if="content.type === 'image' && imageSrc"
      type="button"
      class="tool-image-preview"
      data-testid="tool-image-preview"
      :aria-label="`Open full-size preview of ${title}`"
      @click="openViewer"
    >
      <img :src="imageSrc" :alt="title" />
      <span class="tool-preview-action">View full size <span aria-hidden="true">↗</span></span>
    </button>

    <div v-else-if="content.type === 'diff'" class="tool-code-preview">
      <pre class="acp-diff" data-testid="acp-diff"><code
        v-for="(line, index) in diffLines(content)"
        :key="index"
        class="acp-diff-line"
        :class="line.sign === '-' ? 'acp-diff-del' : 'acp-diff-add'"
      >{{ line.sign }} {{ line.text }}
</code></pre>
      <button
        type="button"
        class="tool-preview-action"
        data-testid="tool-content-expand"
        @click="openViewer"
      >
        Expand diff <span aria-hidden="true">↗</span>
      </button>
    </div>

    <div v-else-if="content.type === 'text' && content.text" class="tool-code-preview">
      <pre class="acp-payload" data-testid="tool-text-preview">{{ content.text }}</pre>
      <button
        type="button"
        class="tool-preview-action"
        data-testid="tool-content-expand"
        @click="openViewer"
      >
        Expand output <span aria-hidden="true">↗</span>
      </button>
    </div>

    <p v-else-if="content.type === 'image'" class="tool-preview-unsupported">
      Image preview unavailable ({{ content.mime_type || 'unknown type' }})
    </p>
  </div>

  <Teleport to="body">
    <div
      v-if="expanded"
      class="tool-viewer-backdrop"
      data-testid="tool-detail-backdrop"
      @mousedown.self="closeViewer"
    >
      <section
        role="dialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        class="tool-viewer"
        data-testid="tool-detail-dialog"
      >
        <header class="tool-viewer-header">
          <div class="tool-viewer-heading">
            <h2 :id="titleId">{{ title }}</h2>
            <p :title="detailLabel">{{ detailLabel }}</p>
          </div>
          <button
            ref="closeButton"
            type="button"
            class="btn-secondary tool-viewer-close"
            data-testid="tool-detail-close"
            @click="closeViewer"
          >
            Close
          </button>
        </header>
        <div class="tool-viewer-body">
          <img
            v-if="content.type === 'image' && imageSrc"
            :src="imageSrc"
            :alt="title"
            class="tool-viewer-image"
            data-testid="tool-detail-image"
          />
          <pre v-else-if="content.type === 'diff'" class="tool-viewer-code acp-diff"><code
            v-for="(line, index) in diffLines(content)"
            :key="index"
            class="acp-diff-line"
            :class="line.sign === '-' ? 'acp-diff-del' : 'acp-diff-add'"
          >{{ line.sign }} {{ line.text }}
</code></pre>
          <pre v-else-if="content.type === 'text'" class="tool-viewer-code acp-payload">{{
            content.text
          }}</pre>
        </div>
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.tool-preview {
  min-width: 0;
}
.tool-image-preview,
.tool-code-preview {
  position: relative;
  width: 100%;
  overflow: hidden;
  border: 1px solid var(--line);
  border-radius: 0.3rem;
  background: var(--code);
}
.tool-image-preview {
  display: flex;
  min-height: 7rem;
  max-height: 18rem;
  cursor: zoom-in;
  align-items: center;
  justify-content: center;
}
.tool-image-preview:hover,
.tool-image-preview:focus-visible {
  border-color: var(--muted);
}
.tool-image-preview img {
  display: block;
  width: 100%;
  max-height: 18rem;
  max-width: 100%;
  object-fit: contain;
}
.tool-preview-action {
  display: flex;
  width: 100%;
  align-items: center;
  justify-content: flex-end;
  gap: 0.35rem;
  border-top: 1px solid var(--line);
  padding: 0.3rem 0.55rem;
  background: color-mix(in srgb, var(--surface) 92%, transparent);
  color: var(--muted);
  font-family: var(--font-sans);
  font-size: 0.6875rem;
  line-height: 1rem;
}
.tool-image-preview .tool-preview-action {
  position: absolute;
  right: 0;
  bottom: 0;
  width: auto;
  border-top: 1px solid var(--line);
  border-left: 1px solid var(--line);
  border-top-left-radius: 0.25rem;
}
button.tool-preview-action:hover,
button.tool-preview-action:focus-visible,
.tool-image-preview:hover .tool-preview-action {
  color: var(--fg);
}
.tool-preview-unsupported {
  margin: 0;
  padding: 0.55rem 0.65rem;
  background: var(--code);
  color: var(--faint);
  font-family: var(--font-mono);
  font-size: 0.75rem;
}
.acp-payload {
  margin: 0;
  max-height: 16rem;
  overflow: auto;
  padding: 0.55rem 0.65rem;
  background: var(--code);
  color: var(--code-fg);
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.2rem;
  white-space: pre-wrap;
}
.acp-diff {
  margin: 0;
  max-height: 16rem;
  overflow: auto;
  padding: 0.4rem 0;
  background: var(--code);
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.2rem;
}
.acp-diff-line {
  display: block;
  padding: 0 0.65rem;
  white-space: pre;
}
.acp-diff-del {
  background: var(--block-soft);
  color: var(--block);
}
.acp-diff-add {
  background: var(--ok-soft);
  color: var(--ok);
}
.tool-viewer-backdrop {
  position: fixed;
  inset: 0;
  z-index: 80;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 1rem;
  background: color-mix(in srgb, var(--canvas) 82%, transparent);
}
.tool-viewer {
  display: flex;
  max-height: calc(100vh - 2rem);
  width: min(88rem, 100%);
  min-height: min(42rem, calc(100vh - 2rem));
  flex-direction: column;
  overflow: hidden;
  border: 1px solid var(--line);
  border-radius: 0.4rem;
  background: var(--surface);
  box-shadow: 0 1.5rem 4rem rgb(0 0 0 / 35%);
}
.tool-viewer-header {
  display: flex;
  flex: none;
  align-items: center;
  gap: 1rem;
  border-bottom: 1px solid var(--line);
  padding: 0.65rem 0.75rem;
  background: var(--rail);
}
.tool-viewer-heading {
  min-width: 0;
}
.tool-viewer-heading h2,
.tool-viewer-heading p {
  overflow: hidden;
  margin: 0;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tool-viewer-heading h2 {
  color: var(--fg);
  font-family: var(--font-mono);
  font-size: 0.8125rem;
  font-weight: 600;
}
.tool-viewer-heading p {
  margin-top: 0.1rem;
  color: var(--faint);
  font-family: var(--font-mono);
  font-size: 0.6875rem;
}
.tool-viewer-close {
  margin-left: auto;
  flex: none;
  padding: 0.3rem 0.65rem;
  font-size: 0.75rem;
}
.tool-viewer-body {
  min-height: 0;
  flex: 1;
  overflow: auto;
  background: var(--code);
}
.tool-viewer-image {
  display: block;
  min-width: 100%;
  max-width: none;
  height: auto;
  margin: auto;
}
.tool-viewer-code {
  min-width: 100%;
  max-height: none;
  min-height: 100%;
  border-radius: 0;
}

@media (max-width: 640px) {
  .tool-viewer-backdrop {
    padding: 0;
  }
  .tool-viewer {
    max-height: 100vh;
    min-height: 100vh;
    border: 0;
    border-radius: 0;
  }
}
</style>
