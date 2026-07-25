<script setup lang="ts">
import { ref, computed, nextTick, watch } from 'vue';
import { useRouter } from 'vue-router';
import type {
  AgentMetadata,
  LaunchOverrides as LaunchOverrideValues,
  LaunchSelection,
  Profile,
  ResolvedLaunch,
  Session,
  WeaverEvent,
} from '../types';
import { ApiError, handoffSession, listAgents, listProfiles, resolveSessionHandoff } from '../api';
import {
  messageOf,
  conversationState,
  lifecycleActions,
  signalChips,
  quietTags,
  autoArchiveDisabled,
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
import LaunchOverrides from './LaunchOverrides.vue';
import ProfileSelector from './ProfileSelector.vue';
import ResolvedLaunchSummary from './ResolvedLaunchSummary.vue';

// The session page header — one compact chrome block shared by both the detail
// view and the file browser, so the "where am I / what is this" context never
// vanishes when you cross into Files.
//
//   row 1  ← sessions · title · current state / freshness · signals · Details
//   row 2  the agent's current-state message, when present
//
// Identity, launch metadata, associations, tags, status history, lifecycle
// actions, and the optional editor live in Details instead of permanently
// occupying the work surface.
const props = withDefaults(
  defineProps<{ ws: Session; events?: WeaverEvent[]; ideEnabled?: boolean }>(),
  {
    events: () => [],
    ideEnabled: false,
  },
);
const emit = defineEmits<{ reload: []; openEditor: [] }>();

const router = useRouter();
const fleetHref = computed(() => {
  if (props.ws.class === 'automation') {
    return {
      path: '/',
      query:
        props.ws.status === 'archived'
          ? { view: 'automation', history: 'true' }
          : { view: 'automation' },
    };
  }
  return props.ws.status === 'archived' ? { path: '/', query: { history: 'true' } } : '/';
});
const fleetLabel = computed(() => {
  if (props.ws.class === 'automation') {
    return props.ws.status === 'archived' ? '← automation history' : '← automation';
  }
  return props.ws.status === 'archived' ? '← history' : '← sessions';
});
// The detail page's subject is the session itself, so a successful Remove has to
// leave: route back to the fleet list rather than reload a page that is gone.
const actions = useSessionActions(
  () => props.ws.id,
  () => emit('reload'),
  () => router.push(fleetHref.value),
);
const { busy, notice, error, rename, clearTag, setAutoArchiveDisabled, run } = actions;

// The lifecycle verbs the ⋯ manage menu offers — the same policy the fleet
// list's row menu renders, so the two surfaces can't drift.
const lifecycle = computed(() => lifecycleActions(props.ws));

const showDetails = ref(false);
const detailsButton = ref<HTMLButtonElement | null>(null);
type DetailsCloseReason = 'toggle' | 'escape' | 'outside' | 'navigation' | 'action';

function closeDetails(reason: DetailsCloseReason) {
  if (!showDetails.value) return;
  showDetails.value = false;
  resetHandoff();
  if (reason === 'escape') void nextTick(() => detailsButton.value?.focus());
}

function toggleDetails() {
  if (showDetails.value) closeDetails('toggle');
  else showDetails.value = true;
}

function openEditor() {
  closeDetails('action');
  emit('openEditor');
}

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

// Derived conversation state (glyph + label + tone) stays visible in the compact
// first row. The text/glyph carry meaning independently of color.
const conv = computed(() => conversationState(props.ws));
const toneClass = computed(() => TONE_TEXT[conv.value.tone]);
const statusMessage = computed(() => messageOf(props.ws));
const lastActivity = computed(() => timeAgo(props.ws.last_activity_at));
// The loud signal chips: the agent's own `attention` and a watch's
// `triage`, each individually deletable. Their presence is what "needs a human"
// means here; clearing a chip DELETEs that tag (there is no "Mark OK" verb).
const signals = computed(() => signalChips(props.ws));
const quiet = computed(() => quietTags(props.ws));
const keepsSession = computed(() => autoArchiveDisabled(props.ws));

interface SurfacedLink {
  href: string;
  label: string;
  source: string;
}

const URL_RE = /https?:\/\/[^\s)\]>"'`]+/g;
const TEXT_KEYS = ['note', 'text', 'message'] as const;
const surfacedLinks = computed<SurfacedLink[]>(() => {
  const links: SurfacedLink[] = [];
  const seen = new Set<string>();
  const pushText = (text: unknown, source: string) => {
    if (typeof text !== 'string') return;
    for (const raw of text.match(URL_RE) ?? []) {
      const href = raw.replace(/[.,;:]+$/, '');
      const key = href.replace(/\/+$/, '');
      if (seen.has(key)) continue;
      seen.add(key);
      links.push({ href, label: href.replace(/^https?:\/\//, ''), source });
      if (links.length === 12) return;
    }
  };

  pushText(statusMessage.value, 'current status');
  for (const event of [...props.events].reverse()) {
    for (const key of TEXT_KEYS) {
      pushText(event.data?.[key], timeAgo(event.created_at));
      if (links.length === 12) return links;
    }
  }
  return links;
});

function statusEventLine(event: WeaverEvent): string {
  const data = event.data ?? {};
  if (event.kind === 'status') return `Lifecycle → ${String(data.status ?? 'unknown')}`;
  const key = String(data.key ?? 'tag');
  const value = String(data.value ?? '');
  const note = typeof data.note === 'string' && data.note ? ` — ${data.note}` : '';
  if (key === 'attention' && data.by === 'agent') return `${value || 'ok'}${note}`;
  return value ? `${key} → ${value}${note}` : `${key} cleared`;
}

const statusTrail = computed(() =>
  props.events
    .filter((event) => event.kind === 'status' || event.kind === 'tag')
    .slice(-8)
    .reverse()
    .map((event) => ({
      id: event.id,
      line: statusEventLine(event),
      when: timeAgo(event.created_at),
    })),
);

// Provider handoff is an ACP-only, between-turn server operation. The manage
// menu exposes the profile picker for a live ACP fleet session; the endpoint is
// still authoritative when a turn starts between paint and submit.
const handoffOpen = ref(false);
const handoffAgents = ref<AgentMetadata[]>([]);
const handoffProfiles = ref<Profile[]>([]);
const handoffProfile = ref('');
const handoffOverrides = ref<LaunchOverrideValues>({});
const handoffResolved = ref<ResolvedLaunch | null>(null);
const lastHandoffResolved = ref<ResolvedLaunch | null>(null);
const handoffResolving = ref(false);
const handoffBusy = ref(false);
const handoffError = ref('');
const canHandoff = computed(
  () => props.ws.protocol === 'acp' && ['running', 'orphaned', 'error'].includes(props.ws.status),
);
const handoffSelection = computed<LaunchSelection>(() => ({
  profile: handoffProfile.value || 'default',
  overrides: { ...handoffOverrides.value },
}));
const unchangedHandoff = computed(() => {
  const resolved = handoffResolved.value;
  return (
    resolved?.agent === props.ws.agent_kind &&
    resolved.model === props.ws.model &&
    resolved.effort === props.ws.effort &&
    resolved.mode === props.ws.launch_mode &&
    resolved.selection.profile === props.ws.profile &&
    resolved.profile_revision === props.ws.profile_revision
  );
});

let handoffResolveRequest = 0;
function resetHandoff() {
  ++handoffResolveRequest;
  handoffOpen.value = false;
  handoffAgents.value = [];
  handoffProfiles.value = [];
  handoffProfile.value = '';
  handoffOverrides.value = {};
  handoffResolved.value = null;
  lastHandoffResolved.value = null;
  handoffResolving.value = false;
  handoffError.value = '';
}

async function resolveHandoff() {
  const request = ++handoffResolveRequest;
  handoffResolved.value = null;
  if (!handoffOpen.value || !handoffProfiles.value.length) return;
  handoffResolving.value = true;
  handoffError.value = '';
  try {
    const preview = await resolveSessionHandoff(props.ws.id, handoffSelection.value);
    if (request === handoffResolveRequest) {
      handoffResolved.value = preview;
      lastHandoffResolved.value = preview;
    }
  } catch (cause) {
    if (request === handoffResolveRequest) handoffError.value = (cause as Error).message;
  } finally {
    if (request === handoffResolveRequest) handoffResolving.value = false;
  }
}

watch(handoffSelection, () => void resolveHandoff(), { deep: true });

async function toggleHandoff() {
  if (handoffOpen.value) {
    resetHandoff();
    return;
  }
  handoffOpen.value = true;
  ++handoffResolveRequest;
  handoffResolved.value = null;
  handoffError.value = '';
  try {
    const [metadata, profiles] = await Promise.all([listAgents(), listProfiles()]);
    handoffAgents.value = metadata.agents.filter((agent) => agent.supports_acp);
    handoffProfiles.value = profiles.filter((profile) => profile.class === props.ws.class);
    handoffProfile.value = handoffProfiles.value.some(
      (profile) => profile.name === props.ws.profile,
    )
      ? props.ws.profile
      : (handoffProfiles.value[0]?.name ?? '');
    handoffOverrides.value = {};
    await resolveHandoff();
  } catch (cause) {
    handoffError.value = (cause as Error).message;
  }
}

function chooseHandoffProfile(profile: string) {
  handoffProfile.value = profile;
  handoffOverrides.value = {};
}

async function submitHandoff() {
  const preview = handoffResolved.value;
  if (handoffBusy.value || unchangedHandoff.value || !preview?.valid) return;
  handoffBusy.value = true;
  handoffError.value = '';
  try {
    await handoffSession(props.ws.id, {
      selection: handoffSelection.value,
      expected_profile_revision: preview.profile_revision,
      expected_resolver_revision: preview.resolver_revision,
    });
    handoffOpen.value = false;
    closeDetails('action');
    notice.value = `Handed off to ${preview.agent}.`;
    window.dispatchEvent(new CustomEvent('loom:acp-handoff', { detail: { id: props.ws.id } }));
    await emit('reload');
  } catch (cause) {
    const preview = cause instanceof ApiError ? cause.body.preview : undefined;
    const message = (cause as Error).message;
    if (preview && typeof preview === 'object') {
      handoffResolved.value = preview as ResolvedLaunch;
      lastHandoffResolved.value = handoffResolved.value;
    } else {
      handoffResolved.value = null;
      await resolveHandoff();
    }
    handoffError.value = message;
  } finally {
    handoffBusy.value = false;
  }
}
</script>

<template>
  <header class="mb-1 py-0.5">
    <!-- Row 1 — location, title, current state, and operational controls. -->
    <div class="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1">
      <router-link
        :to="fleetHref"
        class="flex min-h-7 shrink-0 items-center text-sm text-muted hover:text-fg"
        >{{ fleetLabel }}</router-link
      >
      <input
        v-if="editing"
        ref="inputEl"
        v-model="draft"
        class="min-w-0 flex-1 rounded bg-input px-2 py-1 text-lg font-semibold outline-none focus:ring-1 ring-accent"
        @keydown.enter.prevent="commit"
        @keydown.esc.prevent="cancel"
        @blur="commit"
      />
      <div v-else class="group flex min-w-[10rem] flex-1 items-center gap-1.5">
        <h1 class="min-w-0 truncate text-lg font-semibold tracking-tight">
          {{ ws.branch.title || ws.branch.name }}
        </h1>
        <button
          type="button"
          class="min-h-7 shrink-0 rounded px-1 text-xs text-faint opacity-0 transition-opacity hover:bg-subtle hover:text-fg focus-visible:opacity-100 group-hover:opacity-100"
          title="Rename"
          aria-label="Rename session"
          @click="startEdit"
        >
          ✎
        </button>
      </div>

      <div class="ml-auto flex min-w-0 flex-wrap items-center justify-end gap-1.5">
        <span
          data-testid="conversation-state"
          :class="toneClass"
          class="whitespace-nowrap text-xs"
          role="status"
        >
          {{ conv.glyph }} {{ conv.label }}
        </span>
        <span v-if="lastActivity" class="text-faint" aria-hidden="true">·</span>
        <span v-if="lastActivity" class="whitespace-nowrap font-mono text-2xs text-faint">{{
          lastActivity
        }}</span>

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

        <!-- Details keeps lifecycle actions and identity metadata nearby without
             turning either into permanent workbench chrome. -->
        <div class="relative">
          <button
            ref="detailsButton"
            type="button"
            :aria-expanded="showDetails"
            aria-controls="session-details-popover"
            class="min-h-7 rounded px-2 text-xs font-medium text-muted hover:bg-subtle hover:text-fg"
            @click="toggleDetails"
          >
            Details ⋯
          </button>
          <SessionDetailsPopover :ws="ws" :open="showDetails" @close="closeDetails">
            <template #actions>
              <div class="space-y-1">
                <button
                  v-if="ideEnabled"
                  type="button"
                  data-testid="action-open-editor"
                  class="block w-full rounded px-2 py-1.5 text-left text-fg transition-colors hover:bg-subtle"
                  @click="openEditor"
                >
                  <span class="block text-xs font-medium">Open editor</span>
                  <span class="block text-2xs text-faint"
                    >Open the worktree in the side panel.</span
                  >
                </button>
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
                  class="min-w-0 space-y-3 rounded border border-line bg-input p-2"
                  data-testid="handoff-form"
                  @submit.prevent="submitHandoff"
                >
                  <p class="text-2xs font-semibold uppercase tracking-wider text-muted">
                    Target profile
                  </p>
                  <ProfileSelector
                    :profiles="handoffProfiles"
                    :model-value="handoffProfile"
                    layout="list"
                    :disabled="handoffBusy"
                    @update:model-value="chooseHandoffProfile"
                  />
                  <LaunchOverrides
                    v-model="handoffOverrides"
                    :agents="handoffAgents"
                    :resolved="handoffResolved"
                    :fallback="lastHandoffResolved"
                    :disabled="
                      handoffBusy || handoffResolving || Boolean(handoffResolved?.policy.strict)
                    "
                  />
                  <ResolvedLaunchSummary :resolved="handoffResolved" :loading="handoffResolving" />
                  <p class="text-2xs text-faint">
                    Starts the replacement with this session's goal and conversation history.
                  </p>
                  <p v-if="handoffError" class="text-xs text-block" role="alert">
                    {{ handoffError }}
                  </p>
                  <button
                    type="submit"
                    class="btn-primary px-2.5 py-1 text-xs"
                    :disabled="
                      handoffBusy || handoffResolving || unchangedHandoff || !handoffResolved?.valid
                    "
                  >
                    {{ handoffBusy ? 'Handing off…' : 'Hand off now' }}
                  </button>
                </form>
                <button
                  v-if="ws.status !== 'archived'"
                  type="button"
                  data-testid="action-auto-archive"
                  :disabled="!!busy"
                  class="block w-full rounded px-2 py-1.5 text-left text-fg transition-colors hover:bg-subtle disabled:opacity-60"
                  @click="setAutoArchiveDisabled(!keepsSession)"
                >
                  <span class="block text-xs font-medium">
                    {{
                      busy === 'auto-archive'
                        ? 'Saving…'
                        : keepsSession
                          ? 'Enable auto-archive'
                          : 'Disable auto-archive'
                    }}
                  </span>
                  <span class="block text-2xs text-faint">
                    {{
                      keepsSession
                        ? 'Allow automatic cleanup again.'
                        : 'Keep this session until you archive it.'
                    }}
                  </span>
                </button>
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
            <template #context>
              <div class="space-y-3">
                <nav class="flex flex-wrap gap-2 text-xs" aria-label="Session resources">
                  <router-link
                    :to="`/s/${ws.id}/artifacts`"
                    class="text-accent hover:underline"
                    @click="closeDetails('navigation')"
                    >Artifacts</router-link
                  >
                  <router-link
                    v-if="ws.branch.open_issue_count"
                    :to="{
                      path: '/issues',
                      query: { repo_root: ws.branch.repo_root, branch: ws.branch.branch },
                    }"
                    class="text-accent hover:underline"
                    @click="closeDetails('navigation')"
                    >{{ ws.branch.open_issue_count }} open issue{{
                      ws.branch.open_issue_count === 1 ? '' : 's'
                    }}</router-link
                  >
                </nav>

                <details v-if="ws.branch.goal" class="rounded border border-line bg-input">
                  <summary
                    class="min-h-7 cursor-pointer px-2 py-1 text-xs font-medium text-muted hover:text-fg"
                  >
                    Goal / prompt
                  </summary>
                  <p
                    class="max-h-40 overflow-auto whitespace-pre-wrap border-t border-line px-2 py-1.5 text-xs text-muted"
                    data-testid="session-goal-context"
                  >
                    {{ ws.branch.goal }}
                  </p>
                </details>

                <details v-if="surfacedLinks.length" class="rounded border border-line bg-input">
                  <summary
                    class="min-h-7 cursor-pointer px-2 py-1 text-xs font-medium text-muted hover:text-fg"
                  >
                    Surfaced links ({{ surfacedLinks.length }})
                  </summary>
                  <ul
                    data-testid="session-links"
                    class="max-h-32 space-y-1 overflow-y-auto border-t border-line px-2 py-1.5 text-xs"
                  >
                    <li v-for="link in surfacedLinks" :key="link.href" class="min-w-0">
                      <a
                        :href="link.href"
                        target="_blank"
                        rel="noopener noreferrer"
                        class="block truncate text-accent hover:underline"
                        >{{ link.label }}</a
                      >
                      <span class="text-2xs text-faint">{{ link.source }}</span>
                    </li>
                  </ul>
                </details>

                <GithubAssociations :ws="ws" @reload="emit('reload')" />

                <div v-if="quiet.length" class="flex flex-wrap items-center gap-1.5">
                  <TagPill
                    v-for="tag in quiet"
                    :key="tag.key"
                    :tag="tag"
                    :busy="busy === `tag:${tag.key}`"
                    @clear="clearTag"
                  />
                </div>

                <div v-if="statusTrail.length">
                  <h4 class="mb-1 text-2xs font-semibold uppercase tracking-wider text-muted">
                    Status history
                  </h4>
                  <ol class="space-y-1.5">
                    <li
                      v-for="entry in statusTrail"
                      :key="entry.id"
                      class="flex items-start gap-2 text-xs"
                    >
                      <span class="min-w-0 flex-1 text-muted">{{ entry.line }}</span>
                      <span class="shrink-0 font-mono text-2xs text-faint">{{ entry.when }}</span>
                    </li>
                  </ol>
                </div>
              </div>
            </template>
          </SessionDetailsPopover>
        </div>
      </div>
    </div>

    <!-- Row 2 — one current-state line, sans and bounded. Full history is in
         Details so status prose cannot take over the workbench. -->
    <p
      v-if="statusMessage"
      class="mt-0.5 truncate text-[13px] leading-snug text-muted"
      data-testid="status-message"
      role="status"
    >
      {{ statusMessage }}
    </p>

    <!-- Write feedback (rename / clear tag / archive). Inline so it travels
         with the header on every surface. -->
    <p v-if="error" class="mt-1 text-xs text-block" role="alert">{{ error }}</p>
    <p v-else-if="notice" class="mt-1 text-xs text-accent" role="status">{{ notice }}</p>
  </header>
</template>
