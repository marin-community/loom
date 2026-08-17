<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { getSessionGithubAccess, setSessionGithubAccess } from '../api';
import type { Session, SessionGithubAccess } from '../types';

// The Details popover for the page header. Holds everything low-frequency so it
// stays out of the always-visible header run, yet reachable from any scroll
// position. Two stacked sections:
//   • lifecycle actions, injected by the header via the #actions slot
//   • identity / machine metadata (id, branch, base, terminal, worktree, github)
//
// Actions come first, under a heading: they are the reason a human opens this.
// The complete variable-height body shares one bounded scroller so expanded
// context and handoff controls cannot push later lifecycle actions off-screen.
const props = defineProps<{ ws: Session; open: boolean }>();
const emit = defineEmits<{ close: [reason: 'escape' | 'outside'] }>();
const panel = ref<HTMLElement | null>(null);
const githubAccess = ref<SessionGithubAccess[]>([]);
const githubRepository = ref('');
const githubAccessError = ref('');
const githubAccessBusy = ref(false);

async function loadGithubAccess() {
  try {
    githubAccess.value = await getSessionGithubAccess(props.ws.id);
    githubAccessError.value = '';
  } catch (error) {
    githubAccessError.value = (error as Error).message;
  }
}

async function updateGithubAccess(mode: SessionGithubAccess['mode']) {
  const repository = githubRepository.value.trim();
  if (!repository) return;
  githubAccessBusy.value = true;
  try {
    await setSessionGithubAccess(props.ws.id, repository, mode);
    githubRepository.value = '';
    await loadGithubAccess();
  } catch (error) {
    githubAccessError.value = (error as Error).message;
  } finally {
    githubAccessBusy.value = false;
  }
}

function close(reason: 'escape' | 'outside') {
  emit('close', reason);
}

// This is a nonmodal popover: a pointer press elsewhere light-dismisses it but
// is not swallowed, so the outside control still receives the same press. The
// trigger is exempt because its own click toggles the state.
function onOutsidePointerDown(event: PointerEvent) {
  const target = event.target;
  if (!(target instanceof Element) || panel.value?.contains(target)) return;
  if (target.closest('[aria-controls="session-details-popover"]')) return;
  close('outside');
}

watch(
  () => [props.open, props.ws.id] as const,
  async ([open]) => {
    document.removeEventListener('pointerdown', onOutsidePointerDown, true);
    if (!open) return;
    void loadGithubAccess();
    await nextTick();
    if (!props.open) return;
    document.addEventListener('pointerdown', onOutsidePointerDown, true);
    const first = panel.value?.querySelector<HTMLElement>(
      'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled)',
    );
    (first ?? panel.value)?.focus();
  },
);
onBeforeUnmount(() => document.removeEventListener('pointerdown', onOutsidePointerDown, true));
</script>

<template>
  <div v-if="open" class="relative">
    <!-- Height is capped to the viewport (the button sits ~3rem from the top;
         ~7rem also clears the status bar with slack). One bounded body scroller
         keeps every expanded section and lifecycle action reachable. -->
    <div
      id="session-details-popover"
      ref="panel"
      data-testid="details-popover"
      role="region"
      aria-label="Session details and actions"
      tabindex="-1"
      class="session-details-popover absolute right-0 z-20 mt-1 flex max-h-[calc(100vh-7rem)] w-[min(20rem,calc(100vw-2rem))] flex-col overflow-hidden rounded border border-line bg-surface p-1 shadow-lg outline-none"
      @keydown.esc.stop.prevent="close('escape')"
    >
      <div
        data-testid="details-scroll"
        class="min-h-0 flex-1 overflow-y-auto overscroll-contain p-2"
      >
        <!-- Lifecycle actions (Adopt / Recover / Archive / Remove), supplied by
             the header — first, because they are what this menu is for. -->
        <div>
          <h3 class="mb-1 px-2 text-2xs font-semibold uppercase tracking-wider text-muted">
            Actions
          </h3>
          <slot name="actions" />
        </div>

        <div v-if="$slots.context" class="mt-3 border-t border-line px-2 pt-3">
          <slot name="context" />
        </div>

        <div class="mt-3 border-t border-line px-2 pt-3">
          <h3 class="mb-2 text-2xs font-semibold uppercase tracking-wider text-muted">
            GitHub access
          </h3>
          <ul v-if="githubAccess.length" class="mb-2 space-y-1 text-xs text-muted">
            <li v-for="grant in githubAccess" :key="grant.repository" class="flex gap-2">
              <code class="min-w-0 flex-1 break-all">{{ grant.repository }}</code>
              <span>{{ grant.mode }}</span>
            </li>
          </ul>
          <p v-else class="mb-2 text-xs text-faint">No explicit overrides.</p>
          <input
            v-model="githubRepository"
            class="mb-2 w-full rounded border border-line bg-canvas px-2 py-1 text-xs"
            placeholder="owner/repository"
            aria-label="GitHub repository"
            @keydown.enter.prevent="updateGithubAccess('write')"
          />
          <div class="flex gap-2">
            <button
              type="button"
              class="rounded border border-line px-2 py-1 text-xs text-muted hover:text-default"
              :disabled="githubAccessBusy || !githubRepository.trim()"
              @click="updateGithubAccess('write')"
            >
              Grant write
            </button>
            <button
              type="button"
              class="rounded border border-line px-2 py-1 text-xs text-muted hover:text-default"
              :disabled="githubAccessBusy || !githubRepository.trim()"
              @click="updateGithubAccess('none')"
            >
              Revoke
            </button>
          </div>
          <p v-if="githubAccessError" class="mt-2 text-xs text-danger">{{ githubAccessError }}</p>
        </div>

        <h3
          class="mb-1 mt-3 border-t border-line px-2 pt-3 text-2xs font-semibold uppercase tracking-wider text-muted"
        >
          Details
        </h3>
        <dl class="space-y-2 px-2 text-xs">
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">id</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.id }}</dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">branch</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.branch.branch }}</dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">base</dt>
            <dd class="min-w-0 break-all font-mono text-muted">base {{ ws.branch.base_branch }}</dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">terminal</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.term_session }}</dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">worktree</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.work_dir }}</dd>
          </div>
          <div v-if="ws.github_repo" class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">github</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.github_repo }}</dd>
          </div>
          <div v-if="ws.created_by" class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">created by</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.created_by }}</dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">runtime</dt>
            <dd class="min-w-0 break-all text-muted">
              {{ ws.agent_kind }}<template v-if="ws.model"> · {{ ws.model }}</template
              ><template v-if="ws.effort"> · {{ ws.effort }}</template>
            </dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">profile</dt>
            <dd class="min-w-0 break-all font-mono text-muted">
              {{ ws.profile }} · v{{ ws.profile_revision }}
            </dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">policy</dt>
            <dd class="min-w-0 break-all font-mono text-muted">
              {{ ws.launch_mode || 'default' }}
            </dd>
          </div>
          <div class="flex gap-2">
            <dt class="w-16 shrink-0 text-faint">source</dt>
            <dd class="min-w-0 break-all font-mono text-muted">{{ ws.origin }} · {{ ws.class }}</dd>
          </div>
        </dl>
      </div>
    </div>
  </div>
</template>

<style scoped>
@media (max-width: 639px) {
  .session-details-popover {
    position: fixed;
    inset: auto 0.5rem 3.75rem;
    z-index: 60;
    width: auto;
    max-height: calc(100dvh - 4.5rem);
    margin-top: 0;
    border-radius: 0.5rem 0.5rem 0 0;
  }
}
</style>
