// Terminal appearance: the single place that turns the `terminal.*` server
// settings into concrete xterm.js options (palette, font stack, size). Both the
// live terminal (`AgentTerminal.vue`) and the settings preview
// (`AppearancePanel.vue`) resolve through here, so what the preview shows is
// exactly what a real terminal renders.

import type { ITheme } from '@xterm/xterm';
import { get } from '../api';
import type { SettingView } from '../types';

// Terminal palettes, selected by the `terminal.theme` setting. The dark palette
// keeps xterm's own ANSI colours (they already assume a dark terminal) but sits
// on the UI's recessed-panel tone rather than pure black, so the pane reads as
// part of the workbench instead of a hole in it. The light palette (Solarized
// Light) must supply its own foreground, cursor, and full 16-colour set,
// because xterm's defaults are unreadable on a light background.
export const DARK_THEME: ITheme = { background: '#181818' };
export const LIGHT_THEME: ITheme = {
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

export type ThemeToken = 'dark' | 'light';
export const DEFAULT_THEME: ThemeToken = 'dark';

export function themeFor(name: string | undefined): ITheme {
  return name === 'light' ? LIGHT_THEME : DARK_THEME;
}

// Font tokens the `terminal.font` setting stores, mapped to concrete stacks. The
// bundled faces (imported in main.ts) lead their stack; the platform monospace
// stack is the last-resort fallback and the whole of the `system` choice.
export type FontToken = 'plex' | 'jetbrains' | 'system';
export const DEFAULT_FONT: FontToken = 'plex';

const SYSTEM_STACK = 'ui-monospace, SFMono-Regular, Menlo, Consolas, "DejaVu Sans Mono", monospace';

export const FONT_STACKS: Record<FontToken, string> = {
  plex: `"IBM Plex Mono", ${SYSTEM_STACK}`,
  jetbrains: `"JetBrains Mono", ${SYSTEM_STACK}`,
  system: SYSTEM_STACK,
};

// Human labels + a representative face to load before measuring, keyed by token.
// Consumed by the Appearance picker so each option previews in its own font.
export const FONT_CHOICES: { token: FontToken; label: string; face: string | null }[] = [
  { token: 'plex', label: 'IBM Plex Mono', face: 'IBM Plex Mono' },
  { token: 'jetbrains', label: 'JetBrains Mono', face: 'JetBrains Mono' },
  { token: 'system', label: 'System monospace', face: null },
];

export function fontStack(token: string | undefined): string {
  return FONT_STACKS[(token as FontToken) ?? DEFAULT_FONT] ?? FONT_STACKS[DEFAULT_FONT];
}

// Font size, in CSS pixels (xterm's `fontSize`). Clamp what we apply to a
// legible range so a stray edit (or a stale value) can't render the terminal
// unusable.
export const DEFAULT_FONT_SIZE = 13;
export const MIN_FONT_SIZE = 8;
export const MAX_FONT_SIZE = 24;

export function clampFontSize(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_FONT_SIZE;
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, Math.round(n)));
}

export interface TerminalConfig {
  themeToken: string;
  theme: ITheme;
  fontToken: string;
  fontFamily: string;
  fontSize: number;
}

function valueOf(settings: SettingView[], key: string, fallback: string): string {
  return settings.find((s) => s.key === key)?.value?.trim() || fallback;
}

// Turn a settings snapshot into resolved xterm options. Pure — pass live
// `/settings` values or in-flight drafts; the preview uses the same path.
export function resolveTerminalConfig(settings: SettingView[]): TerminalConfig {
  const themeToken = valueOf(settings, 'terminal.theme', DEFAULT_THEME);
  const fontToken = valueOf(settings, 'terminal.font', DEFAULT_FONT);
  const fontSize = clampFontSize(
    Number(valueOf(settings, 'terminal.font_size', String(DEFAULT_FONT_SIZE))),
  );
  return {
    themeToken,
    theme: themeFor(themeToken),
    fontToken,
    fontFamily: fontStack(fontToken),
    fontSize,
  };
}

// The dark, default-everything config — the safe fallback when `/settings` is
// slow or unreachable, and the base a live terminal opens with before the fetch
// lands.
export function defaultTerminalConfig(): TerminalConfig {
  return resolveTerminalConfig([]);
}

// Best-effort fetch + resolve of the configured terminal appearance. Any
// failure (offline, a stale server missing the keys) falls back to the dark
// defaults.
export async function fetchTerminalConfig(): Promise<TerminalConfig> {
  try {
    const res = (await get('/settings')) as { settings?: SettingView[] };
    return resolveTerminalConfig(res?.settings ?? []);
  } catch {
    return defaultTerminalConfig();
  }
}

// Preload a bundled face so xterm measures real (not fallback) cell metrics
// before it paints — avoids a reflow/misalign on first render or a font switch.
// A missing face or absent FontFaceSet API resolves harmlessly.
export async function ensureFontLoaded(fontFamily: string, size: number): Promise<void> {
  const face = fontFamily.match(/"([^"]+)"/)?.[1];
  if (!face || !document.fonts?.load) return;
  await document.fonts.load(`${size}px "${face}"`).catch(() => {});
}
