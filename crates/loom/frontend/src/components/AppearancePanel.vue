<script setup lang="ts">
import { ref, computed, reactive, onMounted, onUnmounted, watch, nextTick } from 'vue';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { get, patch } from '../api';
import type { SettingView } from '../types';
import {
  FONT_CHOICES,
  fontStack,
  themeFor,
  ensureFontLoaded,
  clampFontSize,
  MIN_FONT_SIZE,
  MAX_FONT_SIZE,
  DEFAULT_FONT_SIZE,
} from '../lib/terminalConfig';

// The terminal Appearance pane: theme, font, and size for the in-browser
// terminal, with a live xterm preview that renders exactly what a real terminal
// will. Self-contained like the other settings panels — it fetches and patches
// `/settings` directly rather than routing through the parent's drafts.

const THEME_KEY = 'terminal.theme';
const FONT_KEY = 'terminal.font';
const SIZE_KEY = 'terminal.font_size';
const KEYS = [THEME_KEY, FONT_KEY, SIZE_KEY];

const THEMES = [
  { token: 'dark', label: 'Dark', bg: '#181818', fg: '#d0d0d0' },
  { token: 'light', label: 'Light', bg: '#fdf6e3', fg: '#586e75' },
];

const SIZE_PRESETS = [12, 13, 14, 16];

// Sample session shown in the preview. Uses only the base-16 SGR colours (plus
// bold/underline) so the swatches and text track whichever palette the theme
// supplies — that's what makes the dark↔light difference legible at a glance.
const SAMPLE = [
  '\x1b[1;32m➜\x1b[0m \x1b[1;36m~/weaver\x1b[0m \x1b[1;30mgit:(\x1b[33mmain\x1b[1;30m)\x1b[0m cargo run',
  '\x1b[32m   Compiling\x1b[0m loom \x1b[90mv0.1.0\x1b[0m',
  '\x1b[1;34mINFO\x1b[0m server listening on \x1b[4;36mhttp://127.0.0.1:8080\x1b[0m',
  '\x1b[31merror\x1b[0m: \x1b[1mconnection reset\x1b[0m — retrying',
  '',
  '  \x1b[30;40m 0 \x1b[31;41m 1 \x1b[32;42m 2 \x1b[33;43m 3 \x1b[34;44m 4 \x1b[35;45m 5 \x1b[36;46m 6 \x1b[37;47m 7 \x1b[0m',
  '  \x1b[90;100m 8 \x1b[91;101m 9 \x1b[92;102m10 \x1b[93;103m11 \x1b[94;104m12 \x1b[95;105m13 \x1b[96;106m14 \x1b[97;107m15 \x1b[0m',
  '\x1b[1;32m➜\x1b[0m \x1b[1;36m~/weaver\x1b[0m ',
].join('\r\n');

const stored = ref<Record<string, SettingView>>({});
const draft = reactive<Record<string, string>>({
  [THEME_KEY]: 'dark',
  [FONT_KEY]: 'plex',
  [SIZE_KEY]: String(DEFAULT_FONT_SIZE),
});
const busy = ref(false);
const error = ref('');
const notice = ref('');
const loaded = ref(false);

const host = ref<HTMLElement | null>(null);
let term: Terminal | null = null;
let fit: FitAddon | null = null;
let observer: ResizeObserver | null = null;
let disposed = false;

const sizeNumber = computed(() => clampFontSize(Number(draft[SIZE_KEY])));

const dirty = computed(() => KEYS.some((k) => draft[k] !== stored.value[k]?.value));
const allDefault = computed(() => KEYS.every((k) => stored.value[k]?.is_default));

function adopt(settings: SettingView[]) {
  const byKey: Record<string, SettingView> = {};
  for (const s of settings) if (KEYS.includes(s.key)) byKey[s.key] = s;
  stored.value = byKey;
  for (const k of KEYS) if (byKey[k]) draft[k] = byKey[k].value;
}

async function load() {
  try {
    const res = (await get('/settings')) as { settings?: SettingView[] };
    adopt(res?.settings ?? []);
    error.value = '';
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    loaded.value = true;
  }
}

function patchBody(reset: boolean): Record<string, string | null> {
  return Object.fromEntries(KEYS.map((k) => [k, reset ? null : draft[k]]));
}

async function commit(body: Record<string, string | null>, done: string) {
  busy.value = true;
  error.value = '';
  notice.value = '';
  try {
    const res = (await patch('/settings', body)) as { settings?: SettingView[] };
    adopt(res?.settings ?? []);
    notice.value = done;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    busy.value = false;
  }
}

async function save() {
  if (!dirty.value) return;
  // Normalise the size to the clamped value we actually apply before saving.
  draft[SIZE_KEY] = String(sizeNumber.value);
  await commit(patchBody(false), 'Saved terminal appearance.');
}

async function resetDefaults() {
  await commit(patchBody(true), 'Reset terminal appearance to defaults.');
}

function nudgeSize(delta: number) {
  draft[SIZE_KEY] = String(clampFontSize(sizeNumber.value + delta));
}

// --- Live preview ----------------------------------------------------------

async function applyPreview() {
  if (!term) return;
  const family = fontStack(draft[FONT_KEY]);
  await ensureFontLoaded(family, sizeNumber.value);
  if (disposed || !term) return;
  term.options.theme = themeFor(draft[THEME_KEY]);
  term.options.fontFamily = family;
  term.options.fontSize = sizeNumber.value;
  fit?.fit();
}

async function buildPreview() {
  if (!host.value || term) return;
  const family = fontStack(draft[FONT_KEY]);
  await ensureFontLoaded(family, sizeNumber.value);
  if (disposed || !host.value) return;
  term = new Terminal({
    convertEol: false,
    disableStdin: true,
    cursorBlink: true,
    fontFamily: family,
    fontSize: sizeNumber.value,
    scrollback: 0,
    theme: themeFor(draft[THEME_KEY]),
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(host.value);
  fit.fit();
  term.write(SAMPLE);
  observer = new ResizeObserver(() => fit?.fit());
  observer.observe(host.value);
}

// Re-render the preview whenever a control changes (before the user saves), so
// the preview is always the pending selection, not the persisted one.
watch(
  () => [draft[THEME_KEY], draft[FONT_KEY], sizeNumber.value],
  () => applyPreview(),
);

onMounted(async () => {
  await load();
  await nextTick();
  await buildPreview();
});

onUnmounted(() => {
  disposed = true;
  observer?.disconnect();
  term?.dispose();
});
</script>

<template>
  <div class="space-y-4">
    <p v-if="notice" class="text-xs text-accent">{{ notice }}</p>
    <p v-if="error" class="text-xs text-block">{{ error }}</p>

    <!-- Live preview: a real xterm instance, so what shows here is exactly what
         a session terminal renders with these settings. -->
    <section class="overflow-hidden rounded-md border border-line bg-surface">
      <div class="flex items-center justify-between border-b border-line px-3 py-2">
        <h3 class="text-sm font-medium">Preview</h3>
        <span class="rounded bg-input px-1.5 py-0.5 font-mono text-2xs text-faint">live</span>
      </div>
      <div class="p-3">
        <div class="overflow-hidden rounded ring-1 ring-line">
          <div ref="host" class="h-44 w-full" data-testid="appearance-preview"></div>
        </div>
      </div>
    </section>

    <!-- Controls -->
    <section class="space-y-4 rounded-md border border-line bg-surface p-4">
      <!-- Theme -->
      <div class="grid gap-3 md:grid-cols-[10rem_minmax(0,1fr)]">
        <div class="min-w-0">
          <div class="text-sm font-medium">Theme</div>
          <p class="mt-0.5 text-xs text-muted">Colour palette for the terminal.</p>
        </div>
        <div class="flex flex-wrap gap-2">
          <button
            v-for="t in THEMES"
            :key="t.token"
            type="button"
            :data-testid="`theme-${t.token}`"
            :aria-pressed="draft[THEME_KEY] === t.token"
            class="flex items-center gap-2 rounded border px-3 py-1.5 text-sm"
            :class="
              draft[THEME_KEY] === t.token
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            @click="draft[THEME_KEY] = t.token"
          >
            <span
              class="h-4 w-4 rounded-full ring-1 ring-line"
              :style="{ background: t.bg, color: t.fg }"
              aria-hidden="true"
            ></span>
            {{ t.label }}
          </button>
        </div>
      </div>

      <!-- Font -->
      <div class="grid gap-3 border-t border-line pt-4 md:grid-cols-[10rem_minmax(0,1fr)]">
        <div class="min-w-0">
          <div class="text-sm font-medium">Font</div>
          <p class="mt-0.5 text-xs text-muted">Typeface, previewed in its own face.</p>
        </div>
        <div class="flex flex-wrap gap-2">
          <button
            v-for="f in FONT_CHOICES"
            :key="f.token"
            type="button"
            :data-testid="`font-${f.token}`"
            :aria-pressed="draft[FONT_KEY] === f.token"
            class="rounded border px-3 py-1.5 text-sm"
            :class="
              draft[FONT_KEY] === f.token
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            :style="{ fontFamily: fontStack(f.token) }"
            @click="draft[FONT_KEY] = f.token"
          >
            {{ f.label }}
          </button>
        </div>
      </div>

      <!-- Size -->
      <div class="grid gap-3 border-t border-line pt-4 md:grid-cols-[10rem_minmax(0,1fr)]">
        <div class="min-w-0">
          <div class="text-sm font-medium">Size</div>
          <p class="mt-0.5 text-xs text-muted">
            Pixel size, {{ MIN_FONT_SIZE }}–{{ MAX_FONT_SIZE }}.
          </p>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          <button
            v-for="p in SIZE_PRESETS"
            :key="p"
            type="button"
            :aria-pressed="sizeNumber === p"
            class="rounded border px-2.5 py-1 text-xs tabular-nums"
            :class="
              sizeNumber === p
                ? 'border-accent bg-accent text-accent-fg'
                : 'border-line bg-input text-muted hover:bg-subtle hover:text-fg'
            "
            @click="draft[SIZE_KEY] = String(p)"
          >
            {{ p }}
          </button>
          <div class="flex items-center overflow-hidden rounded border border-line">
            <button
              type="button"
              class="px-2 py-1 text-sm text-muted hover:bg-subtle hover:text-fg"
              title="Smaller"
              @click="nudgeSize(-1)"
            >
              −
            </button>
            <input
              :value="draft[SIZE_KEY]"
              type="number"
              :min="MIN_FONT_SIZE"
              :max="MAX_FONT_SIZE"
              data-testid="font-size-input"
              class="w-14 bg-input px-2 py-1 text-center text-sm tabular-nums outline-none"
              @input="draft[SIZE_KEY] = ($event.target as HTMLInputElement).value"
            />
            <button
              type="button"
              class="px-2 py-1 text-sm text-muted hover:bg-subtle hover:text-fg"
              title="Larger"
              @click="nudgeSize(1)"
            >
              +
            </button>
          </div>
          <span class="text-2xs text-faint">px</span>
        </div>
      </div>

      <!-- Actions -->
      <div class="flex items-center gap-2 border-t border-line pt-4">
        <button
          class="btn-primary px-3 py-1.5 text-xs disabled:opacity-50"
          :disabled="busy || !dirty || !loaded"
          @click="save"
        >
          Save
        </button>
        <button
          class="btn-secondary px-3 py-1.5 text-xs disabled:opacity-50"
          :disabled="busy || allDefault || !loaded"
          title="Reset theme, font, and size to their defaults"
          @click="resetDefaults"
        >
          Reset to defaults
        </button>
        <span v-if="dirty" class="text-2xs text-faint"
          >Unsaved changes — applies to terminals opened after saving.</span
        >
      </div>
    </section>
  </div>
</template>
