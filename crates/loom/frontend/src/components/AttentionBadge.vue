<script setup lang="ts">
import { computed } from 'vue';

// The ONE resolved attention signal — does this session need me? Driven by
// `effectiveAttention`: the louder of the agent's own `attention` tag and a
// non-stale overlooker `triage` mark. This is the single reserved loud signal in
// the UI; `ok` is intentionally a quiet ghost chip that recedes so only the loud
// amber/red states pop. Colors come from semantic tokens (attn/block) so they
// auto-swap light/dark.
//
// When an overlooker raised it (`raisedBy === 'triage'`) the badge carries the
// ⊙ "watched" glyph and names the overlooker in the tooltip; a stale triage mark
// — the session has moved on since it was set — is faded and flagged, so an
// hour-old "looks stuck" never lies about a session that has since recovered.
const props = defineProps<{
  level: string;
  raisedBy?: 'none' | 'agent' | 'triage';
  by?: string;
  note?: string;
  stale?: boolean;
}>();

interface Style {
  label: string;
  cls: string;
  dot: string;
}

const styles: Record<string, Style> = {
  ok: {
    label: 'OK',
    cls: 'bg-transparent text-faint ring-1 ring-inset ring-line',
    dot: 'bg-faint',
  },
  attention: {
    label: 'Attention',
    cls: 'bg-attn text-attn-fg',
    dot: 'bg-attn-fg/80',
  },
  blocked: {
    label: 'Blocked',
    cls: 'bg-block text-block-fg',
    dot: 'bg-block-fg/80',
  },
};

const style = computed(
  () => styles[props.level] ?? { label: props.level, cls: 'bg-transparent text-faint ring-1 ring-inset ring-line', dot: 'bg-faint' },
);

// An overlooker raised it: mark the badge as watched (⊙) instead of the agent's
// plain dot, and attribute it.
const fromOverlooker = computed(() => props.raisedBy === 'triage');

const tooltip = computed(() => {
  if (props.level === 'ok') return style.value.label;
  if (fromOverlooker.value) {
    const who = props.by && props.by !== 'manual' ? ` (${props.by})` : '';
    const base = props.note
      ? `Overlooker${who}: ${props.note}`
      : `Raised by overlooker${who}`;
    return props.stale ? `${base} — stale, session has moved on` : base;
  }
  return props.note || style.value.label;
});
</script>

<template>
  <span
    :class="[style.cls, stale ? 'opacity-50' : '']"
    data-testid="attention-badge"
    :data-level="level"
    :data-raised-by="raisedBy ?? 'agent'"
    :data-stale="stale ? 'true' : 'false'"
    :title="tooltip"
    class="inline-flex items-center gap-1.5 rounded px-1.5 py-0.5 text-2xs font-medium uppercase tracking-wide"
  >
    <span v-if="fromOverlooker" aria-hidden="true">⊙</span>
    <span v-else :class="style.dot" class="h-1.5 w-1.5 rounded-full"></span>
    {{ style.label }}
  </span>
</template>
