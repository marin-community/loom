<script setup lang="ts">
import MarkdownView from './MarkdownView.vue';
import HtmlDocumentView from './HtmlDocumentView.vue';

defineProps<{
  id: string;
  path: string;
  kind: string;
  content: string;
  rawUrl?: string;
  title?: string;
}>();
</script>

<template>
  <div class="h-full w-full">
    <slot v-if="kind === 'markdown'" name="markdown">
      <MarkdownView :id="id" :path="path" :source="content" />
    </slot>
    <HtmlDocumentView v-else-if="kind === 'html'" :content="content" />
    <div
      v-else-if="kind === 'image'"
      class="flex h-full w-full items-center justify-center overflow-auto bg-surface p-4"
    >
      <img
        :src="rawUrl"
        :alt="title || path"
        class="max-h-full max-w-full object-contain"
        data-testid="artifact-image"
      />
    </div>
    <p v-else-if="kind === 'download'" class="p-6 text-sm text-muted">
      Preview unavailable for binary files and text larger than 2 MiB.
      <a :href="rawUrl" :download="path.split('/').pop()" class="underline">Download file</a>
    </p>
    <pre
      v-else
      class="h-full w-full overflow-auto whitespace-pre-wrap break-words p-4 font-mono text-[13px] leading-5 text-fg"
    ><code>{{ content }}</code></pre>
  </div>
</template>
