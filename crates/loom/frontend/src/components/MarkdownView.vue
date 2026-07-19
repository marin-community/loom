<script setup lang="ts">
import { h, Fragment } from 'vue';
import { useRouter } from 'vue-router';
import { renderTokens } from '../markdown-render';
import { useMarkdownDoc, routeDocLink } from '../lib/markdownDoc';
import type { IssueRefStatus } from '../types';

// Rendered Markdown preview, as a real Vue tree — markdown-it tokens walked into
// vnodes (`markdown-render.ts`), not an `innerHTML` blob. GitHub-flavoured output
// (tables, task lists, inline images) plus the smartdoc projection (`#N` issue
// refs → live status chips, `artifact:<name>` → deep links); fenced code and
// mermaid are the CodeBlock / MermaidBlock leaf components, which lazy-load their
// heavy renderer. This component just parses + hosts; the build pipeline and
// link routing are the shared `markdownDoc` helpers, and the comment layer lives
// in ArtifactDocument (which renders the same tokens and interleaves threads).
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

// The shared build pipeline: parse → tokens/ctx, told to announce the rendered
// <article> to any host once it paints.
const { body, error, tokens, ctx } = useMarkdownDoc(props, (el) => emit('rendered', el));

// The vnode body, recreated on every render so no vnode is ever reused across
// renders (fresh nodes; Vue keeps child component state via keys). Reads the
// reactive `tokens`/`ctx`, so it re-renders when a build lands.
const RenderedBody = () => (ctx.value ? h(Fragment, renderTokens(tokens.value, ctx.value)) : null);

function onClick(e: MouseEvent) {
  routeDocLink(e, router, body.value);
}

// Let a host that measures the rendered prose reach the <article> directly.
defineExpose({ body });
</script>

<template>
  <div class="h-full w-full overflow-auto bg-surface">
    <p
      v-if="error"
      class="m-4 rounded border border-block-line bg-block-soft p-3 text-sm text-block"
    >
      {{ error }}
    </p>
    <article ref="body" class="markdown-body mx-auto max-w-3xl px-6 py-5" @click="onClick">
      <RenderedBody />
    </article>
  </div>
</template>
