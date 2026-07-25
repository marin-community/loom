<script setup lang="ts">
import type { LaunchSource, ResolvedLaunch } from '../types';

defineProps<{
  resolved: ResolvedLaunch | null;
  loading?: boolean;
}>();

function sourceLabel(source: LaunchSource): string {
  if (source === 'launch_override') return 'launch override';
  if (source === 'agent_default') return 'agent default';
  if (source === 'origin_default') return 'launch origin';
  if (source === 'policy_default') return 'policy default';
  return 'profile';
}
</script>

<template>
  <section
    class="rounded-md border border-line bg-surface p-3"
    data-testid="resolved-launch-summary"
    aria-live="polite"
  >
    <div class="mb-2 flex items-center justify-between gap-3">
      <div>
        <h3 class="text-sm font-semibold text-fg">Resolved launch</h3>
        <p class="text-xs text-faint">The immutable snapshot the server will launch.</p>
      </div>
      <span v-if="loading" class="text-xs text-faint">Resolving…</span>
      <span
        v-else-if="resolved"
        class="rounded px-1.5 py-0.5 text-2xs"
        :class="resolved.valid ? 'bg-ok-soft text-ok' : 'bg-block-soft text-block'"
      >
        {{ resolved.valid ? 'ready' : 'needs attention' }}
      </span>
    </div>

    <div v-if="resolved" class="space-y-3">
      <dl class="grid gap-2 sm:grid-cols-2">
        <div
          v-for="field in ['agent', 'model', 'effort', 'protocol', 'mode', 'class'] as const"
          :key="field"
          class="rounded bg-input px-2 py-1.5"
        >
          <dt class="text-2xs uppercase tracking-wider text-faint">{{ field }}</dt>
          <dd class="mt-0.5 flex items-center justify-between gap-2 text-xs">
            <code class="truncate">{{ resolved[field] || 'agent default' }}</code>
            <span
              :data-testid="`provenance-${field}`"
              class="shrink-0 rounded bg-subtle px-1.5 py-0.5 text-2xs text-muted"
            >
              {{ sourceLabel(resolved.provenance[field]) }}
            </span>
          </dd>
        </div>
      </dl>

      <div class="flex flex-wrap gap-1.5 text-2xs text-muted">
        <span class="meta-chip">profile r{{ resolved.profile_revision }}</span>
        <span class="meta-chip">{{ resolved.policy.environment.length }} env names</span>
        <span class="meta-chip"
          >{{ resolved.policy.mcp_policy.capability_sets.length }} MCP sets</span
        >
        <span v-if="resolved.policy.strict" class="meta-chip">strict</span>
        <span v-if="resolved.policy.restricted" class="meta-chip">restricted</span>
        <span v-if="resolved.policy.env_clear" class="meta-chip">environment cleared</span>
        <span class="meta-chip">
          idle {{ resolved.policy.idle_archive_secs }}s ·
          {{ sourceLabel(resolved.provenance.idle_archive_secs) }}
        </span>
        <span class="meta-chip">
          {{ resolved.policy.turn_budget }} turns ·
          {{ sourceLabel(resolved.provenance.turn_budget) }}
        </span>
        <span class="meta-chip">
          {{
            resolved.capacity.maximum == null
              ? `${resolved.capacity.active} active · unlimited`
              : `${resolved.capacity.active}/${resolved.capacity.maximum} active`
          }}
        </span>
      </div>

      <ul v-if="resolved.errors.length" class="space-y-1 text-xs text-block">
        <li v-for="message in resolved.errors" :key="message">• {{ message }}</li>
      </ul>
    </div>
    <p v-else-if="!loading" class="text-xs text-muted">
      Select a profile to preview its concrete runtime and policy.
    </p>
  </section>
</template>
