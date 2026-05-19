<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { get, post } from '../api';
import type { Workspace } from '../types';
import StatusBadge from '../components/StatusBadge.vue';

const workspaces = ref<Workspace[]>([]);
const error = ref('');
const showForm = ref(false);
const goal = ref('');
const repo = ref('');
const creating = ref(false);
let timer: number | undefined;

async function load() {
  try {
    workspaces.value = (await get('/workspaces')) as Workspace[];
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  }
}

async function create() {
  if (!goal.value.trim() || !repo.value.trim()) return;
  creating.value = true;
  try {
    await post('/workspaces', { goal: goal.value, cwd: repo.value });
    goal.value = '';
    showForm.value = false;
    await load();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

onMounted(() => {
  load();
  timer = window.setInterval(load, 3000);
});
onUnmounted(() => clearInterval(timer));
</script>

<template>
  <div>
    <div class="flex items-center justify-between mb-4">
      <h1 class="text-xl font-semibold">Workspaces</h1>
      <button
        class="rounded bg-emerald-700 hover:bg-emerald-600 px-3 py-1.5 text-sm font-medium"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New workspace' }}
      </button>
    </div>

    <form
      v-if="showForm"
      class="mb-5 rounded border border-neutral-800 bg-neutral-900 p-4 space-y-3"
      @submit.prevent="create"
    >
      <div>
        <label class="block text-xs text-neutral-400 mb-1">Repository path (on the server)</label>
        <input
          v-model="repo"
          placeholder="/home/you/code/project"
          class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600"
        />
      </div>
      <div>
        <label class="block text-xs text-neutral-400 mb-1">Goal</label>
        <input
          v-model="goal"
          placeholder="Add a /health endpoint"
          class="w-full rounded bg-neutral-800 px-2 py-1.5 text-sm outline-none focus:ring-1 ring-emerald-600"
        />
      </div>
      <button
        type="submit"
        :disabled="creating"
        class="rounded bg-emerald-700 hover:bg-emerald-600 px-3 py-1.5 text-sm font-medium disabled:opacity-50"
      >
        {{ creating ? 'Creating…' : 'Create' }}
      </button>
    </form>

    <p v-if="error" class="mb-4 text-sm text-red-400">{{ error }}</p>

    <p v-if="!workspaces.length" class="text-neutral-500 text-sm">
      No workspaces yet.
    </p>

    <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <router-link
        v-for="w in workspaces"
        :key="w.id"
        :to="`/w/${w.id}`"
        data-testid="workspace-card"
        :data-workspace-id="w.id"
        class="block rounded border border-neutral-800 bg-neutral-900 p-4 hover:border-neutral-600 transition"
      >
        <div class="flex items-center justify-between gap-2 mb-2">
          <span class="font-medium truncate">{{ w.name }}</span>
          <StatusBadge :status="w.status" />
        </div>
        <p class="text-sm text-neutral-300 line-clamp-2 mb-2">{{ w.goal }}</p>
        <p v-if="w.description" class="text-xs text-neutral-500 line-clamp-3 mb-2">
          {{ w.description }}
        </p>
        <div class="text-xs text-neutral-600 font-mono">{{ w.id }} · {{ w.branch }}</div>
      </router-link>
    </div>
  </div>
</template>
