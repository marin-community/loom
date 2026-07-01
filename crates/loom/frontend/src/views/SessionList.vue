<script setup lang="ts">
import { ref, computed, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { Session } from '../types';
import StatusBadge from '../components/StatusBadge.vue';
import SignalChip from '../components/SignalChip.vue';
import IdleChip from '../components/IdleChip.vue';
import TagPill from '../components/TagPill.vue';
import GithubStatus from '../components/GithubStatus.vue';
import NewSessionDrawer from '../components/NewSessionDrawer.vue';
import { timeAgo } from '../lib/time';
import { effectiveAttention, idleTag, lifecycleDot, messageOf, priorityRank, quietTags, signalChips } from '../lib/sessionState';
import { useFleet } from '../lib/sessionsStore';
import { del } from '../api';

// Named so App.vue's <keep-alive :include> matches it — the fleet list stays
// alive across navigation (instant return, no refetch flash, no re-animate).
defineOptions({ name: 'SessionList' });

// The shared fleet snapshot — polled once for the whole app (App.vue), so this
// view paints from cache the instant it mounts (and, kept alive, stays painted
// across navigation — no refetch flash, no re-animate). `refresh()` forces an
// immediate re-pull after a write (create / clear tag).
const { sessions, refresh } = useFleet();

// Attention filter — the dashboard's "which sessions need me?" control. The
// URL query is the source of truth (`/?filter=attention`, the status bar's
// deep-link): the buttons write it via router.replace and the ref follows, so
// the view is shareable and survives reload/back-forward.
type AttentionFilter = 'all' | 'attention' | 'ok';
const route = useRoute();
const router = useRouter();
function filterFromQuery(q: unknown): AttentionFilter {
  return q === 'attention' || q === 'ok' ? q : 'all';
}
const filter = ref<AttentionFilter>(filterFromQuery(route.query.filter));
// The component is reused when only the query changes (status-bar click while
// already on the fleet list), so track it.
watch(
  () => route.query.filter,
  (q) => (filter.value = filterFromQuery(q)),
);
// Button clicks update the query (replace, not push — filter flips shouldn't
// pollute history); the watcher above folds it back into the ref.
function setFilter(f: AttentionFilter) {
  filter.value = f;
  router.replace({ query: { ...route.query, filter: f === 'all' ? undefined : f } });
}

// Archived sessions are torn-down workstreams: kept for reference but clutter
// the live fleet view. Hide them by default; a reveal chip brings them back.
// They still show when there's nothing else to look at (an all-archived fleet),
// so the list never reads as empty while archived rows exist.
const showArchived = ref(false);

const archivedCount = computed(
  () => sessions.value.filter((s) => s.status === 'archived').length,
);

// The archived-aware, filtered set to display (membership only — the tree below
// imposes the order). Archived rows are hidden by default; a reveal chip brings
// them back, and they always show when there's nothing else, so the list never
// reads empty while archived rows exist. Individual attention rows aren't pinned
// to the top — threading keeps related work grouped — but a whole thread that
// contains attention floats up (see treeRows), and attention rows carry their
// loud signal chip, so they stay easy to spot.
const visibleSessions = computed<Session[]>(() => {
  const all = sessions.value;
  const live = all.filter((s) => s.status !== 'archived');
  const base = showArchived.value || live.length === 0 ? all : live;
  if (filter.value === 'attention') return base.filter((s) => effectiveAttention(s).level !== 'ok');
  if (filter.value === 'ok') return base.filter((s) => effectiveAttention(s).level === 'ok');
  return base;
});

// Counts reflect the full fleet (NOT the archived-hidden view) so the filter
// chips read the true picture; effectiveAttention() already forces archived → ok
// and ignores stale overlooker marks, keeping "needs attention" honest.
const counts = computed(() => {
  const c = { all: sessions.value.length, attention: 0, ok: 0 };
  for (const s of sessions.value) {
    if (effectiveAttention(s).level === 'ok') c.ok += 1;
    else c.attention += 1; // 'attention' and 'blocked' both need a human
  }
  return c;
});

// One flattened tree row: the session, its depth, and the gutter guides drawn to
// its left. `verticals[i]` is true when an ancestor's line keeps running down
// past this row; `isLast` picks the connector (└ vs ├). Depth 0 (top-level)
// draws no gutter, so a flat fleet with no sub-sessions looks exactly as before.
interface TreeRow {
  session: Session;
  depth: number;
  verticals: boolean[];
  isLast: boolean;
}

// Group the visible sessions into threads: each hangs under the session that
// launched it (`parent_id`, a branch id), with top-level sessions under an
// implicit root. A parent always sits directly above its children. Threads stay
// grouped, but a thread containing attention floats to the top: roots and
// siblings sort by their subtree's urgency first (blocked above attention above
// ok), then newest-first within the same urgency — so a blocked/attention child
// can never sink to the bottom of the fleet. A parent that's filtered/archived
// out of the visible set (or was never tracked) drops its orphaned children up.
const treeRows = computed<TreeRow[]>(() => {
  const list = visibleSessions.value;
  const present = new Set(list.map((s) => s.branch.id));
  const children = new Map<string, Session[]>();
  const roots: Session[] = [];
  for (const s of list) {
    const pid = s.parent_id;
    if (pid && pid !== s.branch.id && present.has(pid)) {
      const arr = children.get(pid);
      if (arr) arr.push(s);
      else children.set(pid, [s]);
    } else {
      roots.push(s);
    }
  }
  // Render rank for a single session: blocked is louder than attention is louder
  // than the calm default, and a parked row (waiting on an external reviewer —
  // nothing for the user to do) sinks below it. Used to float urgent threads to
  // the top and sink parked ones to the bottom. Uses the resolved signal (agent's
  // own report or a non-stale overlooker mark) so a triaged thread floats too —
  // matching visibleSessions/counts above.
  const rankOf = (s: Session): number => priorityRank(s);
  // Memoized max render rank across a session's whole subtree (itself + all
  // descendants), so a thread surfaces at the urgency of its loudest member — and
  // a parked parent with live (rank-0) children stays at the calm default rather
  // than sinking the whole thread. The cycle guard mirrors walk()'s: a parent
  // link loop can't spin us forever.
  const rankCache = new Map<string, number>();
  const ranking = new Set<string>();
  const subtreeRank = (s: Session): number => {
    const cached = rankCache.get(s.branch.id);
    if (cached !== undefined) return cached;
    if (ranking.has(s.branch.id)) return rankOf(s); // mid-cycle: just self
    ranking.add(s.branch.id);
    let max = rankOf(s);
    for (const kid of children.get(s.branch.id) ?? []) {
      max = Math.max(max, subtreeRank(kid));
    }
    ranking.delete(s.branch.id);
    rankCache.set(s.branch.id, max);
    return max;
  };
  // Newest-first within a sibling group, matching the dashboard's default feel.
  const byNewest = (a: Session, b: Session) =>
    b.created_at < a.created_at ? -1 : b.created_at > a.created_at ? 1 : 0;
  // Urgent subtree first, then newest-first as the tie-break. When no session
  // carries attention every rank is 0 and this collapses to plain byNewest.
  const byUrgencyThenNewest = (a: Session, b: Session) =>
    subtreeRank(b) - subtreeRank(a) || byNewest(a, b);

  const rows: TreeRow[] = [];
  const seen = new Set<string>(); // guard against any cycle in the parent links
  const walk = (node: Session, depth: number, verticals: boolean[], isLast: boolean) => {
    if (seen.has(node.branch.id)) return;
    seen.add(node.branch.id);
    rows.push({ session: node, depth, verticals, isLast });
    const kids = [...(children.get(node.branch.id) ?? [])].sort(byUrgencyThenNewest);
    kids.forEach((kid, i) => {
      const last = i === kids.length - 1;
      // The implicit root isn't a drawn column, so a top-level node's children
      // start with no ancestor lines; deeper, append this node's continuation.
      const childVerticals = depth === 0 ? [] : [...verticals, !isLast];
      walk(kid, depth + 1, childVerticals, last);
    });
  };
  const sortedRoots = [...roots].sort(byUrgencyThenNewest);
  sortedRoots.forEach((r, i) => walk(r, 0, [], i === sortedRoots.length - 1));
  return rows;
});
const error = ref('');
const showForm = ref(false);

// A quiet pill's × clears that tag, then refreshes the row. The tag write
// surface is the same DELETE the detail page uses.
const clearingTag = ref('');
async function clearTag(sessionId: string, key: string) {
  clearingTag.value = `${sessionId}:${key}`;
  try {
    await del(`/sessions/${sessionId}/tags/${encodeURIComponent(key)}`);
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    clearingTag.value = '';
  }
}

async function handleCreated() {
  try {
    await refresh();
  } catch (e) {
    error.value = (e as Error).message;
  }
}
</script>

<template>
  <div class="px-5 py-3">
    <!-- One toolbar line: the view label, the attention filter, the archived
         reveal, and the primary action — no page-hero heading (the rail
         already says where you are; the h1 stays for a11y + tests). -->
    <div class="mb-3 flex min-h-7 flex-wrap items-center gap-2.5">
      <h1 class="text-2xs font-semibold uppercase tracking-wider text-muted">Sessions</h1>

      <!-- Attention filter: jump straight to the sessions that need a human.
           Each segment pairs a label with its count in a small pill so the
           number reads as a count, not a suffix glued to the word. -->
      <div v-if="sessions.length" class="inline-flex overflow-hidden rounded border border-line text-xs">
        <button
          v-for="opt in (['all', 'attention', 'ok'] as const)"
          :key="opt"
          type="button"
          :data-testid="`filter-${opt}`"
          :class="[
            'flex items-center gap-1.5 border-l border-line px-2.5 py-1 font-medium transition-colors first:border-l-0',
            filter === opt
              ? 'bg-accent text-accent-fg'
              : 'bg-input text-muted hover:bg-subtle hover:text-fg',
          ]"
          @click="setFilter(opt)"
        >
          {{ opt === 'all' ? 'All' : opt === 'attention' ? 'Needs attention' : 'OK' }}
          <span
            :class="[
              'rounded-full px-1.5 text-2xs leading-4',
              filter === opt ? 'bg-accent-fg/20 text-accent-fg' : 'bg-subtle text-faint',
            ]"
          >{{ counts[opt] }}</span>
        </button>
      </div>

      <!-- Archived live below the fold: a quiet chip reveals/hides them. -->
      <button
        v-if="archivedCount"
        type="button"
        :aria-pressed="showArchived"
        :class="[
          'rounded border border-line px-2.5 py-1 text-xs text-muted transition-colors',
          showArchived ? 'bg-subtle text-fg' : 'bg-input hover:bg-subtle',
        ]"
        @click="showArchived = !showArchived"
      >
        {{ showArchived ? 'Hide' : 'Show' }} {{ archivedCount }} archived
      </button>

      <!-- Toggles the create form. Closed → primary (accent) call-to-action;
           open → a neutral "Cancel" so it never reads as a second primary
           action competing with the form's own Create button. -->
      <button
        :class="[
          'ml-auto px-2.5 py-1 text-xs font-medium',
          showForm ? 'btn-secondary' : 'btn-primary',
        ]"
        @click="showForm = !showForm"
      >
        {{ showForm ? 'Cancel' : 'New session' }}
      </button>
    </div>


    <NewSessionDrawer
      v-if="showForm"
      @close="showForm = false"
      @created="handleCreated"
    />

    <p v-if="error" class="mb-4 text-sm text-block">{{ error }}</p>

    <div
      v-if="!sessions.length"
      class="rounded-md border border-dashed border-line p-6 text-center"
    >
      <p class="text-sm text-muted">No sessions yet.</p>
      <p class="mt-1 text-xs text-faint">
        Launch one with <strong>New session</strong>, or
        <code>loom session launch "&lt;goal&gt;"</code>.
      </p>
    </div>

    <!--
      One row per session. Left→right: an optional tree gutter threading child
      sessions under their launcher, the dominant title with its loud signal
      chips (attention/triage, each deletable) and quiet tag pills alongside, a
      muted current-state line, a neutral lifecycle pill (shown only for
      off-nominal states — running is the silent default), and the mono branch
      ref pushed far-right. Rows are grouped into threads (built in script), with
      attention-carrying threads floated up so the loud chips surface near the
      top. The row itself stays neutral — no full-tile wash — so threading reads
      cleanly; the chip carries the signal. Stagger via --i.
    -->
    <ul v-if="sessions.length" data-testid="session-list" class="fade-in overflow-hidden rounded-md border border-line bg-surface">
      <li
        v-for="{ session: s, depth, verticals, isLast } in treeRows"
        :key="s.id"
        data-testid="session-card"
        :data-session-id="s.id"
        :data-depth="depth"
        :class="[
          'group flex cursor-pointer items-start gap-2.5 border-b border-line px-3 py-2 last:border-0',
          'min-h-11 transition-colors hover:bg-subtle',
        ]"
        @click="$router.push(`/s/${s.id}`)"
      >
        <!-- Tree gutter: threads a child session under the one that launched it.
             Drawn only for nested rows, so a flat fleet is visually unchanged. -->
        <div v-if="depth > 0" class="tree-gutter" aria-hidden="true">
          <span
            v-for="(v, c) in verticals"
            :key="c"
            class="tree-col"
            :class="{ 'tree-col--through': v }"
          ></span>
          <span class="tree-col" :class="isLast ? 'tree-col--elbow' : 'tree-col--tee'"></span>
        </div>

        <!-- Status dot: a calm, scannable hue for the row's resolved state
             (green live · cyan resting · amber/red raised · faint detached). -->
        <span
          class="mt-1.5 h-2 w-2 shrink-0 rounded-full"
          :class="lifecycleDot(s)"
          aria-hidden="true"
        ></span>

        <!-- Title + current-state (the work, in prose). -->
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <router-link
              :to="`/s/${s.id}`"
              class="truncate text-sm font-semibold text-fg hover:text-accent"
              @click.stop
            >
              {{ s.branch.title || s.branch.name }}
            </router-link>
            <!-- Loud signals: the agent's `attention` and an overlooker's
                 `triage`, each a deletable chip. The × clears that tag (calm is
                 its absence) — there is no separate "Mark OK" verb. -->
            <SignalChip
              v-for="chip in signalChips(s)"
              :key="chip.key"
              :chip="chip"
              :busy="clearingTag === `${s.id}:${chip.key}`"
              @clear="(key) => clearTag(s.id, key)"
            />
            <!-- Lifecycle: demoted, neutral, mono pill (StatusBadge). Hidden for
                 the running state — nearly every live row is running, so the pill
                 would just be repeated noise; only off-nominal states show one. -->
            <StatusBadge v-if="s.status !== 'running'" :status="s.status" class="shrink-0" />
            <!-- Soothing idle mark: a calm, neutral chip when the agent is
                 resting (no loud signal). Reassures rather than alarms. -->
            <IdleChip v-if="idleTag(s)" :tag="idleTag(s)!" />
            <!-- Quiet free-form tags: deletable pills, never the loud fill. -->
            <TagPill
              v-for="t in quietTags(s)"
              :key="t.key"
              :tag="t"
              :busy="clearingTag === `${s.id}:${t.key}`"
              @clear="(key) => clearTag(s.id, key)"
            />
          </div>

          <!-- Current-state headline (agent's status message), else the goal.
               On an attention row the goal steps up from faint to muted so the
               metadata doesn't recede next to the loud chip. -->
          <p
            v-if="messageOf(s)"
            class="mt-0.5 truncate text-xs text-muted"
          >
            {{ messageOf(s) }}
          </p>
          <p
            v-if="s.branch.goal"
            class="mt-0.5 truncate text-xs"
            :class="effectiveAttention(s).level === 'ok' ? 'text-faint' : 'text-muted'"
          >
            {{ s.branch.goal }}
          </p>
        </div>

        <!-- Ref: machine identity, mono, pushed far-right and receding. -->
        <div class="shrink-0 text-right">
          <span class="block truncate font-mono text-2xs text-faint">{{ s.branch.branch }}</span>
          <!-- Attribution: who/what launched this session — a subtle provenance
               label on the shared board. Absent for older rows. -->
          <span
            v-if="s.created_by"
            class="block truncate text-2xs text-faint"
            :title="`Launched by ${s.created_by}`"
          >
            by <span class="font-mono">{{ s.created_by }}</span>
          </span>
          <!-- PR snapshot (if any) — a quiet link straight to the GitHub PR. -->
          <GithubStatus v-if="s.branch.github" :gh="s.branch.github" compact class="mt-0.5 justify-end" />
          <router-link
            v-if="s.branch.open_issue_count"
            :to="`/s/${s.id}?tab=overview`"
            class="block font-mono text-2xs text-muted hover:text-accent"
            @click.stop
          >
            {{ s.branch.open_issue_count }} open issue{{ s.branch.open_issue_count === 1 ? '' : 's' }}
          </router-link>
          <span v-if="s.last_activity_at" class="mt-0.5 block font-mono text-2xs text-faint">
            {{ timeAgo(s.last_activity_at) }}
          </span>
        </div>
      </li>
    </ul>
  </div>
</template>
