<script setup lang="ts">
import { ref, onMounted, computed } from 'vue';
import AgentTerminal from './AgentTerminal.vue';
import { get, del } from '../api';

// The session's terminal area: an inner tab strip over the always-mounted agent
// terminal plus zero or more worktree **debug shells**.
//
// The agent terminal is the live agent — it never unmounts (tearing down its
// socket/xterm/WebGL is the worst thing on a terminal-first page), so it sits
// under v-show like the parent's top-level tabs. Each debug shell is a plain
// login shell in the session's worktree for testing/debugging beside the agent
// (run the tests, inspect the diff). A shell is spawned on the backend the first
// time its tab is attached and torn down with the session on archive; closing a
// tab kills it now. Shells survive a reload (their supervisors are detached), so
// we rediscover the open ones on mount.
//
// `shellsOnly` drops the Agent tab entirely: an ACP session is headless (no agent
// TUI to attach an xterm to), so its top-level Shells tab shows the worktree
// shells alone — the escape hatch, not the agent's home.
const props = defineProps<{ id: string; shellsOnly?: boolean }>();

// 'agent' selects the live agent terminal; a number selects that debug shell by
// its backend index. An ACP session has no agent terminal, so it opens on its
// first shell (or the empty state until one is added).
const active = ref<'agent' | number>(props.shellsOnly ? -1 : 'agent');
// Open shell tabs, by backend index. Indices are monotonic per page so a closed
// tab's index is never reused while open; the supervisor is `loom-shell-<id>-<idx>`.
const shells = ref<number[]>([]);
let nextIdx = 0;

const noShells = computed(() => props.shellsOnly && shells.value.length === 0);

async function loadShells() {
  try {
    const idxs = (await get(`/sessions/${props.id}/shells`)) as number[];
    shells.value = [...idxs].sort((a, b) => a - b);
    nextIdx = shells.value.length ? Math.max(...shells.value) + 1 : 0;
    // Land the shells-only view on the first rediscovered shell.
    if (props.shellsOnly && active.value === -1 && shells.value.length) {
      active.value = shells.value[0];
    }
  } catch {
    /* best-effort: a probe failure just opens with no rediscovered shells */
  }
}

function addShell() {
  const idx = nextIdx++;
  shells.value.push(idx);
  active.value = idx;
}

async function closeShell(idx: number) {
  shells.value = shells.value.filter((n) => n !== idx);
  if (active.value === idx) active.value = props.shellsOnly ? (shells.value[0] ?? -1) : 'agent';
  // Kill the backend supervisor so a worktree shell never lingers after its tab
  // is gone (archive also sweeps these; closing is the explicit "I'm done").
  try {
    await del(`/sessions/${props.id}/shell/${idx}`);
  } catch {
    /* already gone */
  }
}

onMounted(loadShells);
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Inner tab strip: Agent (terminal sessions only) + the worktree debug
         shells + a new-shell button. Quiet and compact — it lives inside the work
         area, beneath the main tabs, so it stays visually subordinate to them. -->
    <div class="mb-2 flex items-center gap-1 text-xs">
      <button
        v-if="!shellsOnly"
        type="button"
        data-term-tab="agent"
        class="rounded px-2 py-1"
        :class="active === 'agent' ? 'bg-subtle font-medium text-fg' : 'text-muted hover:text-fg'"
        @click="active = 'agent'"
      >
        Agent
      </button>
      <div
        v-for="(idx, i) in shells"
        :key="idx"
        class="flex items-center rounded"
        :class="active === idx ? 'bg-subtle text-fg' : 'text-muted hover:text-fg'"
      >
        <button type="button" class="py-1 pl-2 pr-1 font-medium" @click="active = idx">
          Shell {{ i + 1 }}
        </button>
        <button
          type="button"
          class="px-1 py-1 text-faint hover:text-block"
          title="Close this shell"
          aria-label="Close shell"
          @click="closeShell(idx)"
        >
          ✕
        </button>
      </div>
      <button
        type="button"
        data-term-tab="add-shell"
        class="rounded px-2 py-1 text-muted hover:bg-subtle hover:text-fg"
        title="Open a shell in the worktree"
        @click="addShell"
      >
        + Shell
      </button>
    </div>

    <div class="min-h-0 flex-1">
      <!-- Agent terminal: always mounted (v-show), never v-if — its host stays in
           the DOM so AgentTerminal's zero-size guard skips the bogus resize while
           hidden and its ResizeObserver re-fits on return. Absent for a headless
           (ACP) session. -->
      <section v-if="!shellsOnly" v-show="active === 'agent'" class="h-full">
        <AgentTerminal :id="props.id" />
      </section>
      <!-- Debug shells: mounted when opened and kept mounted (v-show) so switching
           tabs never drops the PTY; unmounted only when the tab is closed. -->
      <section v-for="idx in shells" v-show="active === idx" :key="idx" class="h-full">
        <AgentTerminal
          :ws-path="`/api/sessions/${props.id}/shell/${idx}/terminal`"
          class="h-full"
        />
      </section>
      <!-- A headless session with no shells open yet: a quiet invitation, so the
           tab isn't a blank void. -->
      <div
        v-if="noShells"
        class="flex h-full items-center justify-center rounded border border-dashed border-line"
      >
        <button
          type="button"
          class="btn-secondary px-3 py-1 text-xs"
          data-testid="acp-open-shell"
          @click="addShell"
        >
          Open a shell in the worktree
        </button>
      </div>
    </div>
  </div>
</template>
