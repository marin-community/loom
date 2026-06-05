<script setup lang="ts">
import { ref, watch, onMounted, nextTick } from 'vue';
import { renderMarkdown, renderMermaid } from '../markdown';
import { theme } from '../theme';

// Rendered Markdown preview: GitHub-flavoured HTML (tables, task lists,
// syntax-highlighted code, inline images) plus mermaid diagrams. The heavy
// renderer is lazy-loaded by `markdown.ts`; this component just drives it and
// hosts the output.
const props = defineProps<{
  /** Session id — used to resolve relative images to the raw-bytes endpoint. */
  id: string;
  /** Repo-relative path of the markdown file (its directory anchors images). */
  path: string;
  /** Raw markdown source. */
  source: string;
}>();

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
    const html = await renderMarkdown(props.source, { sessionId: props.id, filePath: props.path });
    if (mine !== runId) return;
    if (body.value) {
      body.value.innerHTML = html;
      await nextTick();
      await renderMermaid(body.value, theme.value === 'dark');
    }
  } catch (e) {
    if (mine === runId) error.value = (e as Error).message;
  } finally {
    if (mine === runId) rendering.value = false;
  }
}

watch(() => [props.source, props.path], render);
// Re-render on theme flip so mermaid diagrams pick up the matching palette.
watch(theme, render);
onMounted(render);

// Intercept in-page `#` links (heading permalinks, in-doc TOC links): scroll
// the matching element into view rather than letting the browser change the
// hash, which would clobber the SPA's hash-based router.
function onClick(e: MouseEvent) {
  const anchor = (e.target as HTMLElement).closest('a');
  const href = anchor?.getAttribute('href');
  if (!href || !href.startsWith('#')) return;
  e.preventDefault();
  const id = decodeURIComponent(href.slice(1));
  body.value?.querySelector(`[id="${CSS.escape(id)}"]`)?.scrollIntoView({
    behavior: 'smooth',
    block: 'start',
  });
}
</script>

<template>
  <div class="h-full w-full overflow-auto bg-surface">
    <p v-if="error" class="m-4 rounded border border-red-400/40 bg-red-500/10 p-3 text-sm text-red-400">
      {{ error }}
    </p>
    <article ref="body" class="markdown-body mx-auto max-w-3xl px-6 py-5" @click="onClick"></article>
  </div>
</template>
