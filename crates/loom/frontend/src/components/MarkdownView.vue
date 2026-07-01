<script setup lang="ts">
import { ref, watch, onMounted, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import { renderMarkdown, renderMermaid } from '../markdown';
import { theme } from '../theme';
import type { IssueRefStatus } from '../types';

// Rendered Markdown preview: GitHub-flavoured HTML (tables, task lists,
// syntax-highlighted code, inline images) plus mermaid diagrams, with the
// smartdoc projection pass (`#N` issue refs → live status chips,
// `artifact:<name>` → deep links). The heavy renderer is lazy-loaded by
// `markdown.ts`; this component just drives it and hosts the output.
const props = defineProps<{
  /** Session id — used to resolve relative images to the raw-bytes endpoint and
   *  to build `artifact:` deep links. */
  id: string;
  /** Repo-relative path of the markdown file (its directory anchors images). */
  path: string;
  /** Raw markdown source. */
  source: string;
  /** Live status for `#N` issue references, keyed by id-as-string — the
   *  projection map. For an artifact pass `ArtifactView.refs.issues`; for the
   *  goal pass a client-built map from the session's issues. Absent → refs render
   *  as plain text. */
  refs?: Record<string, IssueRefStatus>;
}>();

const router = useRouter();

const emit = defineEmits<{ rendered: [el: HTMLElement | null] }>();

const body = ref<HTMLElement | null>(null);
const error = ref('');
const rendering = ref(false);

// A monotonic token guards against an out-of-order finish: if the file changes
// while an async render is mid-flight, only the latest run gets to paint.
let runId = 0;

async function render() {
  const mine = ++runId;
  rendering.value = true;
  error.value = '';
  try {
    const html = await renderMarkdown(props.source, {
      sessionId: props.id,
      filePath: props.path,
      refs: props.refs,
    });
    if (mine !== runId) return;
    if (body.value) {
      body.value.innerHTML = html;
      await nextTick();
      await renderMermaid(body.value, theme.value === 'dark');
      // Every (re)render — source/refs/theme flip — reshuffles the DOM, so a
      // parent hosting margin comments (ArtifactComments) needs to know to
      // relocate its anchors against the fresh nodes.
      emit('rendered', body.value);
    }
  } catch (e) {
    if (mine === runId) error.value = (e as Error).message;
  } finally {
    if (mine === runId) rendering.value = false;
  }
}

watch(() => [props.source, props.path, props.refs], render, { deep: true });
// Re-render on theme flip so mermaid diagrams pick up the matching palette.
watch(theme, render);
onMounted(render);

// Intercept in-page `#` links (heading permalinks, in-doc TOC links): scroll
// the matching element into view rather than letting the browser change the
// hash, which would clobber the SPA's hash-based router. smartdoc chips/links
// (carrying `data-issue` / `data-artifact`) are internal SPA routes — push them
// through the router so they navigate in-app instead of a full reload.
function onClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a');
  if (!anchor) return;
  const href = anchor.getAttribute('href');
  if (!href) return;
  if (anchor.hasAttribute('data-issue') || anchor.hasAttribute('data-artifact')) {
    e.preventDefault();
    router.push(href);
    return;
  }
  if (!href.startsWith('#')) return;
  e.preventDefault();
  const id = decodeURIComponent(href.slice(1));
  body.value?.querySelector(`[id="${CSS.escape(id)}"]`)?.scrollIntoView({
    behavior: 'smooth',
    block: 'start',
  });
}

// Let a parent (ArtifactComments) reach the rendered <article> directly —
// e.g. to attach a mouseup listener for selection-to-comment.
defineExpose({ body });
</script>

<template>
  <div class="h-full w-full overflow-auto bg-surface">
    <p v-if="error" class="m-4 rounded border border-block-line bg-block-soft p-3 text-sm text-block">
      {{ error }}
    </p>
    <article ref="body" class="markdown-body mx-auto max-w-3xl px-6 py-5" @click="onClick"></article>
  </div>
</template>
