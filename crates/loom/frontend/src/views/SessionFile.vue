<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { getSessionFile, sessionFileRawUrl } from '../api';
import type { OperationOutput } from '../api/generated';
import DocumentPreview from '../components/DocumentPreview.vue';

const props = defineProps<{ id: string; path: string }>();
const file = ref<OperationOutput<'sessions.file'> | null>(null);
const error = ref('');
const source = ref(false);
const rawUrl = computed(() => sessionFileRawUrl(props.id, props.path));
let epoch = 0;

async function load() {
  const mine = ++epoch;
  file.value = null;
  error.value = '';
  source.value = false;
  try {
    const result = await getSessionFile(props.id, props.path);
    if (mine === epoch) file.value = result;
  } catch (e) {
    if (mine === epoch) error.value = (e as Error).message;
  }
}
watch(() => [props.id, props.path], load, { immediate: true });
</script>

<template>
  <section class="flex h-full min-h-0 flex-col bg-surface" data-testid="session-file">
    <header class="flex flex-wrap items-center gap-3 border-b border-line px-4 py-3 text-sm">
      <router-link :to="`/s/${encodeURIComponent(id)}`" class="text-muted hover:text-fg"
        >← Session</router-link
      >
      <h1 class="min-w-0 flex-1 break-all font-mono">{{ path }}</h1>
      <button
        v-if="file?.content != null"
        class="text-muted hover:text-fg"
        @click="source = !source"
      >
        {{ source ? 'Preview' : 'Source' }}
      </button>
      <button class="text-muted hover:text-fg" @click="load">Reload</button>
      <a :href="rawUrl" :download="path.split('/').pop()" class="text-muted hover:text-fg"
        >Download</a
      >
    </header>
    <p v-if="error" role="alert" class="p-4 text-block">{{ error }}</p>
    <DocumentPreview
      v-else-if="file"
      :id="id"
      :path="path"
      :kind="source ? 'text' : file.kind"
      :content="file.content ?? ''"
      :raw-url="rawUrl"
      class="min-h-0 flex-1"
    />
    <p v-else class="p-4 text-muted">Loading…</p>
  </section>
</template>
