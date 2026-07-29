<script setup lang="ts">
import { computed } from 'vue';
import type { Session, SessionSummary } from '../types';
import { effectiveAttention, messageOf, quietTags, signalChips } from '../lib/sessionState';
import { timeAgo } from '../lib/time';
import AgentUsage from './AgentUsage.vue';
import GithubStatus from './GithubStatus.vue';
import KeyHint from './KeyHint.vue';

const props = defineProps<{
  session?: SessionSummary;
  detail?: Session;
  loading?: boolean;
  error?: string;
  position: number;
  total: number;
  selected?: boolean;
  expanded?: boolean;
}>();
defineEmits<{
  open: [event: MouseEvent];
  toggleSelect: [];
  toggleDetails: [];
}>();

const title = computed(
  () => props.session?.branch.title || props.session?.branch.name || 'No session selected',
);
const attention = computed(() => (props.session ? effectiveAttention(props.session) : null));
const statusMessage = computed(() => (props.session ? messageOf(props.session) : ''));
const repoName = computed(() => {
  const path = props.session?.branch.repo_root ?? '';
  return path.split('/').filter(Boolean).at(-1) ?? path;
});
</script>

<template>
  <aside
    data-testid="session-mailbox-preview"
    class="session-mailbox-preview min-h-0 border border-line bg-surface"
    aria-label="Current session preview"
  >
    <header class="terminal-pane-heading">
      <span>SESSION://INSPECT</span>
      <span v-if="total"
        >{{ String(position).padStart(2, '0') }}/{{ String(total).padStart(2, '0') }}</span
      >
      <span v-else>--/--</span>
    </header>

    <template v-if="session">
      <div class="min-h-0 flex-1 overflow-y-auto">
        <section class="border-b border-line px-4 py-4">
          <p class="mb-2 text-2xs uppercase tracking-[0.18em] text-faint">
            <span class="text-accent">loom@local</span>:{{ repoName }}$ inspect
            {{ session.id.slice(0, 8) }}
          </p>
          <h2 class="break-words font-mono text-lg font-semibold leading-6 text-fg">
            {{ title }}
          </h2>
          <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-2xs uppercase">
            <span
              class="inline-flex items-center gap-1.5"
              :class="
                attention?.level === 'blocked'
                  ? 'text-block'
                  : attention?.level === 'attention'
                    ? 'text-attn'
                    : 'text-ok'
              "
            >
              <span class="text-[8px]" aria-hidden="true">◆</span>
              {{ attention?.level ?? 'ok' }}
            </span>
            <span class="text-muted">{{ session.status }}</span>
            <span v-if="session.last_activity_at" class="text-faint">
              {{ timeAgo(session.last_activity_at) }}
            </span>
          </div>
        </section>

        <section v-if="statusMessage || attention?.note" class="terminal-preview-section">
          <h3>LAST SIGNAL</h3>
          <p class="terminal-output-line">
            {{ attention?.note || statusMessage }}
          </p>
        </section>

        <section class="terminal-preview-section">
          <h3>TASK BUFFER</h3>
          <p
            v-if="detail?.branch.goal"
            class="whitespace-pre-wrap font-sans text-sm leading-5 text-fg"
          >
            {{ detail.branch.goal }}
          </p>
          <p v-else-if="loading" class="terminal-loading">fetching session context<span>_</span></p>
          <p v-else-if="error" class="text-xs text-block">{{ error }}</p>
          <p v-else class="text-xs text-faint">No task context available.</p>
        </section>

        <section class="terminal-preview-section">
          <h3>PROCESS</h3>
          <dl class="terminal-kv">
            <dt>repo</dt>
            <dd :title="session.branch.repo_root">{{ repoName }}</dd>
            <dt>branch</dt>
            <dd :title="session.branch.branch">{{ session.branch.branch }}</dd>
            <dt>space</dt>
            <dd>{{ session.placement?.space_name ?? 'unplaced' }}</dd>
            <dt>group</dt>
            <dd>{{ session.placement?.group_name ?? 'unplaced' }}</dd>
            <dt>profile</dt>
            <dd>{{ session.profile || 'default' }}</dd>
            <dt>origin</dt>
            <dd>{{ session.origin }}</dd>
          </dl>
        </section>

        <section
          v-if="signalChips(session).length || quietTags(session).length"
          class="terminal-preview-section"
        >
          <h3>FLAGS</h3>
          <div class="flex flex-wrap gap-1.5">
            <span
              v-for="chip in signalChips(session)"
              :key="chip.key"
              class="terminal-flag"
              :class="chip.level === 'blocked' ? 'text-block' : 'text-attn'"
            >
              {{ chip.key }}={{ chip.level }}
            </span>
            <span v-for="tag in quietTags(session)" :key="tag.key" class="terminal-flag">
              {{ tag.key }}={{ tag.value }}
            </span>
          </div>
        </section>

        <section v-if="session.usage" class="terminal-preview-section">
          <h3>CONTEXT</h3>
          <AgentUsage :usage="session.usage" />
        </section>

        <section v-if="session.branch.github" class="terminal-preview-section">
          <h3>UPSTREAM</h3>
          <GithubStatus :gh="session.branch.github" />
        </section>
      </div>

      <footer class="border-t border-line bg-rail/70 p-2">
        <div class="grid grid-cols-3 gap-1.5">
          <router-link
            :to="`/s/${session.id}`"
            class="terminal-action terminal-action--primary"
            @click="$emit('open', $event)"
          >
            <KeyHint keys="Enter" />
            open
          </router-link>
          <button type="button" class="terminal-action" @click="$emit('toggleSelect')">
            <KeyHint keys="x" />
            {{ selected ? 'unselect' : 'select' }}
          </button>
          <button type="button" class="terminal-action" @click="$emit('toggleDetails')">
            <KeyHint keys="o" />
            {{ expanded ? 'collapse' : 'details' }}
          </button>
        </div>
      </footer>
    </template>

    <div v-else class="flex flex-1 items-center justify-center p-6 text-center text-xs text-faint">
      <p>
        <span class="text-accent">&gt;</span> no session in current buffer<span class="cursor-blink"
          >_</span
        >
      </p>
    </div>
  </aside>
</template>
