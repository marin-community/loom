<script setup lang="ts">
import { ref, computed, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import type { AgentMetadata, Session } from '../types';
import { handoffSession, listAgents } from '../api';
import {
  messageOf,
  conversationState,
  lifecycleActions,
  signalChips,
  quietTags,
  TONE_TEXT,
} from '../lib/sessionState';
import { timeAgo } from '../lib/time';
import { useSessionActions } from '../lib/sessionActions';
import StatusBadge from './StatusBadge.vue';
import SignalChip from './SignalChip.vue';
import TagPill from './TagPill.vue';
import SessionDetailsPopover from './SessionDetailsPopover.vue';
import SessionRemedyButton from './SessionRemedyButton.vue';
import GithubAssociations from './GithubAssociations.vue';

// The session page header — one compact chrome block shared by both the detail
// view and the file browser, so the "where am I / what is this" context never
// vanishes when you cross into Files.
//
//   row 1  ← all · title (inline rename) · [signal chips]ⁱ
//           · lifecycle badge · ⌄ details menu
//   row 2  the agent's current-state message as prose (the point of the page)
//   row 3  repo/branch · agent · PR/issue links · quiet conversation-state + freshness
//
// The old full-width "▶ Working … last activity" strip is gone: when the session
// is calm, its state is a quiet note on row 3; when a loud signal is raised it
// shows up on row 1 as a deletable chip (the agent's `attention` and/or an
// watch's `triage`), and the human clears it with the chip's × — there is
// no separate "Mark OK" control. ⁱ shown only when a signal is actually raised.
const props = defineProps<{ ws: Session }>();
const emit = defineEmits<{ reload: [] }>();

const router = useRouter();
const fleetHref = computed(() =>
  props.ws.class === 'automation' ? { path: '/', query: { view: 'automation' } } : '/',
);
const fleetLabel = computed(() => (props.ws.class === 'automation' ? '← automation' : '← all'));
// The detail page's subject is the session itself, so a successful Remove has to
// leave: route back to the fleet list rather than reload a page that is gone.
const actions = useSessionActions(
  () => props.ws.id,
  () => emit('reload'),
  () => router.push(fleetHref.value),
);
const { busy, notice, error, rename, clearTag, run } = actions;

// The lifecycle verbs the ⋯ manage menu offers — the same policy the fleet
// list's row menu renders, so the two surfaces can't drift.
const lifecycle = computed(() => lifecycleActions(props.ws));

const showDetails = ref(false);

// Inline title rename — the title lives only here, no separate edit box. Click
// the ✎ to edit; Enter/blur commits, Esc cancels. Title is the one branch field
// a human authors; goal and status are agent-authored and read-only elsewhere.
const editing = ref(false);
const draft = ref('');
const inputEl = ref<HTMLInputElement | null>(null);

function current(): string {
  return props.ws.branch.title || props.ws.branch.name;
}

async function startEdit() {
  draft.value = current();
  editing.value = true;
  await nextTick();
  inputEl.value?.focus();
  inputEl.value?.select();
}

function commit() {
  if (!editing.value) return;
  editing.value = false;
  const next = draft.value.trim();
  if (next && next !== current()) rename(next);
}

function cancel() {
  editing.value = false;
}

// The short repo label is the last path segment of the worktree's repo root.
function repoName(p: string): string {
  return p.replace(/\/+$/, '').split('/').pop() || p;
}

// Derived conversation state (glyph + label + tone) for the quiet meta line on
// row 3 — only shown when the session is calm; a loud state lives up on row 1 as
// a chip instead.
const conv = computed(() => conversationState(props.ws));
const toneClass = computed(() => TONE_TEXT[conv.value.tone]);
const statusMessage = computed(() => messageOf(props.ws));
// A quiet per-tier tint for the model name — a small scannable hue on the meta
// line (cyan haiku · green sonnet · violet opus · blue fable), muted otherwise.
const MODEL_TINT: Record<string, string> = {
  haiku: 'text-info',
  sonnet: 'text-ok',
  opus: 'text-agent',
  fable: 'text-accent',
};
const modelTint = computed(() => MODEL_TINT[props.ws.model?.toLowerCase()] ?? 'text-muted');
const lastActivity = computed(() => timeAgo(props.ws.last_activity_at));
// The loud signal chips: the agent's own `attention` and a watch's
// `triage`, each individually deletable. Their presence is what "needs a human"
// means here; clearing a chip DELETEs that tag (there is no "Mark OK" verb).
const signals = computed(() => signalChips(props.ws));
const quiet = computed(() => quietTags(props.ws));

// Provider handoff is an ACP-only, between-turn server operation. The manage
// menu exposes the profile picker for a live ACP fleet session; the endpoint is
// still authoritative when a turn starts between paint and submit.
const handoffOpen = ref(false);
const handoffAgents = ref<AgentMetadata[]>([]);
const handoffAgent = ref('');
const handoffModel = ref('');
const handoffEffort = ref('');
const handoffBusy = ref(false);
const handoffError = ref('');
const canHandoff = computed(
  () => props.ws.protocol === 'acp' && ['running', 'orphaned', 'error'].includes(props.ws.status),
);
const unchangedHandoff = computed(
  () =>
    handoffAgent.value === props.ws.agent_kind &&
    handoffModel.value === props.ws.model &&
    handoffEffort.value === props.ws.effort,
);
const handoffMetadata = computed(() =>
  handoffAgents.value.find((a) => a.kind === handoffAgent.value),
);

async function toggleHandoff() {
  handoffOpen.value = !handoffOpen.value;
  handoffError.value = '';
  if (!handoffOpen.value) return;
  handoffAgent.value = props.ws.agent_kind;
  handoffModel.value = props.ws.model;
  handoffEffort.value = props.ws.effort;
  if (!handoffAgents.value.length) {
    try {
      handoffAgents.value = (await listAgents()).agents.filter((a) => a.supports_acp);
    } catch (e) {
      handoffError.value = (e as Error).message;
    }
  }
}

function chooseHandoffAgent(kind: string) {
  if (kind === handoffAgent.value) return;
  handoffAgent.value = kind;
  handoffModel.value = '';
  handoffEffort.value = '';
}

function chooseHandoffAgentFromEvent(event: Event) {
  chooseHandoffAgent((event.target as HTMLSelectElement).value);
}

async function submitHandoff() {
  if (handoffBusy.value || unchangedHandoff.value) return;
  handoffBusy.value = true;
  handoffError.value = '';
  try {
    await handoffSession(props.ws.id, {
      agent: handoffAgent.value,
      model: handoffModel.value,
      effort: handoffEffort.value,
    });
    handoffOpen.value = false;
    showDetails.value = false;
    notice.value = `Handed off to ${handoffAgent.value}.`;
    window.dispatchEvent(new CustomEvent('loom:acp-handoff', { detail: { id: props.ws.id } }));
    await emit('reload');
  } catch (e) {
    handoffError.value = (e as Error).message;
  } finally {
    handoffBusy.value = false;
  }
}
</script>

<template>
  <header class="mb-1 rounded-r border-l-2 border-transparent py-0.5 pl-3 pr-1">
    <!-- Row 1 — nav, title (inline rename), attention + lifecycle controls -->
    <div class="flex items-center gap-2.5">
      <router-link :to="fleetHref" class="shrink-0 text-sm text-muted hover:text-fg">{{
        fleetLabel
      }}</router-link>
      <input
        v-if="editing"
        ref="inputEl"
        v-model="draft"
        class="min-w-0 flex-1 rounded bg-input px-2 py-1 text-lg font-semibold outline-none focus:ring-1 ring-accent"
        @keydown.enter.prevent="commit"
        @keydown.esc.prevent="cancel"
        @blur="commit"
      />
      <div v-else class="group flex min-w-0 items-center gap-1.5">
        <h1 class="min-w-0 truncate font-serif text-[19px] font-semibold tracking-tight">
          {{ ws.branch.title || ws.branch.name }}
        </h1>
        <button
          type="button"
          class="shrink-0 text-xs text-faint opacity-0 transition-opacity hover:text-fg group-hover:opacity-100"
          title="Rename"
          @click="startEdit"
        >
          ✎
        </button>
      </div>

      <div class="ml-auto flex shrink-0 items-center gap-2">
        <!-- The loud signals, inline: the agent's `attention` and a watch's
             `triage`, each a deletable chip. The × clears that tag (calm is its
             absence) — there is no separate "Mark OK". A watch chip carries
             the ⊙ glyph and fades when stale. -->
        <SignalChip
          v-for="chip in signals"
          :key="chip.key"
          :chip="chip"
          :busy="busy === `tag:${chip.key}`"
          @clear="clearTag"
        />

        <!-- Lifecycle pill only for off-nominal states — running is the silent
             default here just as on the fleet list. -->
        <StatusBadge v-if="ws.status !== 'running'" :status="ws.status" />

        <!-- The remedy, promoted out of the menu and parked against the badge
             that announces the problem: an orphaned session offers Adopt, an
             archived one Recover. Same component the fleet-list row uses, so the
             cure looks and reads the same wherever you meet a stuck session. -->
        <SessionRemedyButton :ws="ws" @changed="emit('reload')" @error="error = $event" />

        <!-- ⋯ manage — the lifecycle actions, with the identity metadata under
             them. Named for what a human comes here to *do*: it used to read
             "⌄ details", which advertised only the metadata, so nobody looking
             to archive or adopt a session ever thought to open it. -->
        <div class="relative">
          <button
            type="button"
            :aria-expanded="showDetails"
            class="rounded px-1.5 py-1 text-xs text-muted hover:bg-subtle hover:text-fg"
            @click="showDetails = !showDetails"
          >
            ⋯ manage
          </button>
          <SessionDetailsPopover :ws="ws" v-model:open="showDetails">
            <template #actions>
              <div class="space-y-1">
                <button
                  v-if="canHandoff"
                  type="button"
                  data-testid="action-handoff"
                  class="block w-full rounded px-2 py-1.5 text-left text-fg transition-colors hover:bg-subtle"
                  @click="toggleHandoff"
                >
                  <span class="block text-xs font-medium">Hand off</span>
                  <span class="block text-2xs text-faint"
                    >Replace the provider; keep work and conversation.</span
                  >
                </button>
                <form
                  v-if="handoffOpen"
                  class="space-y-3 rounded border border-line bg-input p-2"
                  data-testid="handoff-form"
                  @submit.prevent="submitHandoff"
                >
                  <label class="block text-2xs font-semibold uppercase tracking-wider text-muted">
                    Provider
                    <select
                      :value="handoffAgent"
                      class="mt-1 block w-full rounded bg-surface px-2 py-1.5 text-xs font-normal normal-case tracking-normal text-fg"
                      @change="chooseHandoffAgentFromEvent"
                    >
                      <option v-for="a in handoffAgents" :key="a.kind" :value="a.kind">
                        {{ a.label }}
                      </option>
                    </select>
                  </label>
                  <label class="block text-2xs font-semibold uppercase tracking-wider text-muted">
                    Model
                    <select
                      v-model="handoffModel"
                      class="mt-1 block w-full rounded bg-surface px-2 py-1.5 text-xs font-normal normal-case tracking-normal text-fg"
                    >
                      <option value="">Default</option>
                      <option v-for="m in handoffMetadata?.models ?? []" :key="m.id" :value="m.id">
                        {{ m.label }}
                      </option>
                    </select>
                  </label>
                  <label class="block text-2xs font-semibold uppercase tracking-wider text-muted">
                    Effort
                    <select
                      v-model="handoffEffort"
                      class="mt-1 block w-full rounded bg-surface px-2 py-1.5 text-xs font-normal normal-case tracking-normal text-fg"
                    >
                      <option value="">Default</option>
                      <option v-for="e in handoffMetadata?.efforts ?? []" :key="e.id" :value="e.id">
                        {{ e.label }}
                      </option>
                    </select>
                  </label>
                  <p class="text-2xs text-faint">
                    Starts the replacement with this session's goal and conversation history.
                  </p>
                  <p v-if="handoffError" class="text-xs text-block">{{ handoffError }}</p>
                  <button
                    type="submit"
                    class="btn-primary px-2.5 py-1 text-xs"
                    :disabled="handoffBusy || unchangedHandoff || !handoffAgent"
                  >
                    {{ handoffBusy ? 'Handing off…' : 'Hand off now' }}
                  </button>
                </form>
                <button
                  v-for="a in lifecycle"
                  :key="a.verb"
                  type="button"
                  :data-testid="`action-${a.verb}`"
                  :disabled="!!busy"
                  class="block w-full rounded px-2 py-1.5 text-left transition-colors disabled:opacity-60"
                  :class="a.danger ? 'text-block hover:bg-block-soft' : 'text-fg hover:bg-subtle'"
                  @click="run(a.verb)"
                >
                  <span class="block text-xs font-medium">
                    {{ busy === a.verb ? a.busyLabel : a.label }}
                  </span>
                  <span class="block text-2xs text-faint">{{ a.hint }}</span>
                </button>
              </div>
            </template>
          </SessionDetailsPopover>
        </div>
      </div>
    </div>

    <!-- Row 2 — the current-state headline (the agent's "where am I"). Set in the
         serif annotation voice (italic), like the note on each fleet row — full
         foreground, it's the point of the page, not chrome. -->
    <p
      v-if="statusMessage"
      class="mt-0.5 line-clamp-2 font-serif text-[13px] italic leading-snug text-fg"
      data-testid="status-message"
    >
      {{ statusMessage }}
    </p>

    <!-- Quiet tags — free-form, deletable pills (priority, needs-rebase, …),
         never the reserved loud fill. Each × clears that tag. -->
    <div v-if="quiet.length" class="mt-0.5 flex flex-wrap items-center gap-1.5">
      <TagPill
        v-for="t in quiet"
        :key="t.key"
        :tag="t"
        :busy="busy === `tag:${t.key}`"
        @clear="clearTag"
      />
    </div>

    <!-- Row 3 — one quiet meta line: repo/branch · agent, then the calm
         conversation-state + freshness pushed to the right. (When attention is
         raised the state lives loudly up on row 1 instead.) -->
    <div class="mt-1 flex items-center gap-1.5 text-xs">
      <span class="min-w-0 truncate font-mono text-muted">
        {{ repoName(ws.branch.repo_root) }}/{{ ws.branch.name }}
      </span>
      <span class="text-faint">·</span>
      <span class="shrink-0 text-muted">
        {{ ws.agent_kind
        }}<template v-if="ws.model">
          · <span :class="modelTint" class="font-medium">{{ ws.model }}</span></template
        >
      </span>
      <span class="text-faint">·</span>

      <!-- GitHub associations stay visible even when empty. Clicking either
           pill opens its editor; the PR retains automatic discovery as a mode,
           while the issue is the explicit link on this session's tracker. -->
      <GithubAssociations :ws="ws" @reload="emit('reload')" />
      <div class="ml-auto flex shrink-0 items-center gap-1.5">
        <span v-if="!signals.length" data-testid="conversation-state" :class="toneClass">
          {{ conv.glyph }} {{ conv.label }}
        </span>
        <span v-if="!signals.length && lastActivity" class="text-faint">·</span>
        <span v-if="lastActivity" class="font-mono text-faint">{{ lastActivity }}</span>
      </div>
    </div>

    <!-- Write feedback (rename / clear tag / archive). Inline so it travels
         with the header on every surface. -->
    <p v-if="error" class="mt-1 text-xs text-block">{{ error }}</p>
    <p v-else-if="notice" class="mt-1 text-xs text-accent">{{ notice }}</p>
  </header>
</template>
