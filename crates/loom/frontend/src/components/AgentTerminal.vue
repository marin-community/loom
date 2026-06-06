<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebglAddon } from '@xterm/addon-webgl';
import '@xterm/xterm/css/xterm.css';
import { get } from '../api';
import type { SettingView } from '../types';

// A real terminal in the browser: xterm.js bridged over a WebSocket to a
// server-owned PTY running `tmux attach`. The PTY is the single interaction
// surface — keystrokes, keys, and full-screen TUIs all go through here.
//
// Note: this talks to loom on the page origin, so it only works against the
// production build served by loom (the rspack dev server is a different origin
// and the server's same-origin check would reject the upgrade).

const props = defineProps<{ id: string }>();

const host = ref<HTMLElement | null>(null);
type ConnState = 'connecting' | 'open' | 'reconnecting' | 'error';
const state = ref<ConnState>('connecting');
const errorReason = ref('');

let term: Terminal | null = null;
let fit: FitAddon | null = null;
let webgl: WebglAddon | null = null;
let ws: WebSocket | null = null;
let observer: ResizeObserver | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let rafHandle = 0;
let attempt = 0;
let closedByUs = false;
let disposed = false;
let lastCols = 0;
let lastRows = 0;

const OP_INPUT = 0x00;
const OP_RESIZE = 0x01;

// Terminal palettes, selected by the `terminal.theme` server setting. The dark
// palette is the long-standing default — a black background with xterm's own
// ANSI colours, which already assume a dark terminal — so it stays minimal to
// preserve the exact prior appearance. The light palette (Solarized Light) must
// supply its own foreground, cursor, and full 16-colour set, because xterm's
// defaults are unreadable on a light background.
const DARK_THEME: ITheme = { background: '#000000' };
const LIGHT_THEME: ITheme = {
  background: '#fdf6e3',
  foreground: '#586e75',
  cursor: '#586e75',
  cursorAccent: '#fdf6e3',
  selectionBackground: '#eee8d5',
  black: '#073642',
  red: '#dc322f',
  green: '#859900',
  yellow: '#b58900',
  blue: '#268bd2',
  magenta: '#d33682',
  cyan: '#2aa198',
  white: '#eee8d5',
  brightBlack: '#002b36',
  brightRed: '#cb4b16',
  brightGreen: '#586e75',
  brightYellow: '#657b83',
  brightBlue: '#839496',
  brightMagenta: '#6c71c4',
  brightCyan: '#93a1a1',
  brightWhite: '#fdf6e3',
};

function themeFor(name: string | undefined): ITheme {
  return name === 'light' ? LIGHT_THEME : DARK_THEME;
}

// How long to wait for the theme fetch before opening the terminal with the
// dark default anyway. Same-origin localhost answers in single-digit ms; this
// ceiling only matters if the request stalls, so the terminal never hangs
// closed waiting on it.
const THEME_FETCH_TIMEOUT_MS = 1000;

// Best-effort fetch of the configured terminal theme. Any failure (offline,
// stale server without the setting) falls back to the dark default. The caller
// races this against a timeout so a *slow* (not just failed) request can't hold
// the terminal closed either.
async function loadTheme(): Promise<ITheme> {
  try {
    const res = (await get('/settings')) as { settings?: SettingView[] };
    const s = res?.settings?.find((x) => x.key === 'terminal.theme');
    return themeFor(s?.value);
  } catch {
    return DARK_THEME;
  }
}

function wsUrl(): string {
  // http→ws / https→wss on the page origin.
  const base = location.origin.replace(/^http/, 'ws');
  return `${base}/api/sessions/${props.id}/terminal`;
}

function inputFrame(data: string): Uint8Array {
  const bytes = new TextEncoder().encode(data);
  const out = new Uint8Array(bytes.length + 1);
  out[0] = OP_INPUT;
  out.set(bytes, 1);
  return out;
}

function resizeFrame(cols: number, rows: number): Uint8Array {
  const b = new Uint8Array(5);
  b[0] = OP_RESIZE;
  b[1] = (cols >> 8) & 0xff;
  b[2] = cols & 0xff;
  b[3] = (rows >> 8) & 0xff;
  b[4] = rows & 0xff;
  return b;
}

function sendOpen(buf: Uint8Array) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(buf);
}

// Coalesce fit() to one per animation frame; skip when the host is hidden /
// zero-sized (would otherwise ship a bogus 1-row resize); only send a resize
// frame when the geometry actually changed, and only once the socket is open.
function scheduleFit() {
  if (rafHandle) return;
  rafHandle = requestAnimationFrame(() => {
    rafHandle = 0;
    if (disposed || !term || !fit || !host.value) return;
    if (host.value.clientWidth === 0 || host.value.clientHeight === 0) return;
    fit.fit();
    const { cols, rows } = term;
    if (cols < 2 || rows < 2) return;
    if (cols === lastCols && rows === lastRows) return;
    lastCols = cols;
    lastRows = rows;
    sendOpen(resizeFrame(cols, rows));
  });
}

function connect() {
  closedByUs = false;
  state.value = attempt === 0 ? 'connecting' : 'reconnecting';
  const sock = new WebSocket(wsUrl());
  sock.binaryType = 'arraybuffer';
  ws = sock;

  sock.onopen = () => {
    attempt = 0;
    state.value = 'open';
    // Re-establish geometry now that we can send (tmux attach repaints).
    lastCols = 0;
    lastRows = 0;
    scheduleFit();
  };
  sock.onmessage = (ev) => {
    if (disposed || !term) return;
    term.write(new Uint8Array(ev.data as ArrayBuffer));
  };
  sock.onclose = (ev) => {
    if (closedByUs || disposed) return;
    // A rejected upgrade (forbidden / orphaned) arrives as an opaque 1006; a
    // setup failure arrives as 1011 with a reason. Either way we back off and
    // retry — the orphaned case is recovered via the SessionDetail Adopt button.
    if (ev.code === 1011 && ev.reason) errorReason.value = ev.reason;
    scheduleReconnect();
  };
  // onerror is always followed by onclose, where reconnect is handled.
  sock.onerror = () => {};
}

function scheduleReconnect() {
  state.value = 'reconnecting';
  attempt += 1;
  const delay = Math.min(30000, 500 * 2 ** attempt) * (0.5 + Math.random() * 0.5);
  reconnectTimer = setTimeout(connect, delay);
}

function onVisible() {
  if (
    document.visibilityState === 'visible' &&
    ws &&
    ws.readyState === WebSocket.CLOSED &&
    !closedByUs &&
    !disposed
  ) {
    if (reconnectTimer) clearTimeout(reconnectTimer);
    attempt = 0;
    connect();
  }
}

onMounted(async () => {
  if (!host.value) return;
  // Resolve the configured palette before constructing the terminal so it
  // paints in the right theme from the first frame (no dark→light flash on the
  // common fast path). But never let a slow/stalled request hold the terminal
  // closed: race the fetch against a short timeout, open with the dark default
  // if it loses, and upgrade to the configured palette once it lands.
  const themePromise = loadTheme();
  const initialTheme = await Promise.race([
    themePromise,
    new Promise<ITheme>((resolve) => setTimeout(() => resolve(DARK_THEME), THEME_FETCH_TIMEOUT_MS)),
  ]);
  if (disposed || !host.value) return; // unmounted while the fetch was in flight
  term = new Terminal({
    convertEol: false,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
    fontSize: 13,
    scrollback: 5000,
    allowProposedApi: true, // required to activate the unicode11 addon
    theme: initialTheme,
    // Constrain agent-supplied OSC 8 hyperlinks to http(s); reject
    // javascript:/data:/file: which an untrusted agent could otherwise emit.
    linkHandler: {
      activate(_event, uri) {
        try {
          const u = new URL(uri);
          if (u.protocol === 'http:' || u.protocol === 'https:') {
            window.open(uri, '_blank', 'noopener,noreferrer');
          }
        } catch {
          /* ignore unparseable URIs */
        }
      },
    },
  });
  term.open(host.value);

  // If the timeout won the race above, apply the real palette once the fetch
  // resolves. Reference equality holds because the palettes are module-level
  // singletons, so a no-op (configured theme === fallback) doesn't churn the
  // renderer.
  themePromise.then((t) => {
    if (!disposed && term && t !== initialTheme) term.options.theme = t;
  });

  fit = new FitAddon();
  term.loadAddon(fit);

  const uni = new Unicode11Addon();
  term.loadAddon(uni);
  term.unicode.activeVersion = '11';

  // WebGL renderer with a DOM-renderer fallback on context loss / unavailability.
  try {
    const addon = new WebglAddon();
    addon.onContextLoss(() => {
      addon.dispose();
      webgl = null;
    });
    term.loadAddon(addon);
    webgl = addon;
  } catch {
    webgl = null;
  }

  // Keystrokes → PTY. Dropped if the socket isn't open (don't queue stale input
  // into tmux during the connect / reconnect window).
  term.onData((data) => sendOpen(inputFrame(data)));

  // The ResizeObserver fires once immediately on observe() after the first
  // layout — that callback is the authoritative initial size, so there is no
  // separate one-shot open-time fit.
  observer = new ResizeObserver(() => scheduleFit());
  observer.observe(host.value);

  document.addEventListener('visibilitychange', onVisible);
  connect();
});

onUnmounted(() => {
  disposed = true;
  closedByUs = true;
  if (reconnectTimer) clearTimeout(reconnectTimer);
  if (rafHandle) cancelAnimationFrame(rafHandle);
  document.removeEventListener('visibilitychange', onVisible);
  observer?.disconnect();
  if (ws) {
    ws.onclose = null;
    ws.onmessage = null;
    ws.onerror = null;
    ws.close();
  }
  // dispose() tears down loaded addons too; don't double-dispose webgl.
  term?.dispose();
});
</script>

<template>
  <div class="relative h-full min-h-0">
    <div ref="host" class="h-full w-full overflow-hidden rounded bg-code text-code-fg"></div>
    <div
      v-if="state !== 'open'"
      data-testid="term-status"
      class="absolute right-2 top-2 rounded px-2 py-1 text-xs"
      :class="state === 'error' ? 'bg-red-950/90 text-red-300' : 'bg-input/90 text-amber-300'"
    >
      <span v-if="state === 'connecting'">connecting…</span>
      <span v-else-if="state === 'reconnecting'">reconnecting…</span>
      <span v-else>disconnected{{ errorReason ? `: ${errorReason}` : '' }}</span>
    </div>
  </div>
</template>
