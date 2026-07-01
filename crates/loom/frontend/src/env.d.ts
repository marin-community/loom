declare module '*.vue' {
  import type { DefineComponent } from 'vue';
  const component: DefineComponent<Record<string, unknown>, Record<string, unknown>, unknown>;
  export default component;
}

// Side-effect style imports carry no types: our own global stylesheet and
// xterm's bundled CSS (`*.css`), plus the pure-CSS `@fontsource` packages the
// entrypoint pulls in for the app fonts. The bundler (rspack) turns these into
// real assets; here they only need to type-check as opaque modules.
declare module '*.css';
declare module '@fontsource/*';
declare module '@fontsource-variable/*';

// markdown-it-task-lists ships no type declarations; it's a standard
// markdown-it plugin (a `PluginWithOptions`-shaped function).
declare module 'markdown-it-task-lists' {
  import type { PluginWithOptions } from 'markdown-it';
  const plugin: PluginWithOptions<{ enabled?: boolean; label?: boolean; labelAfter?: boolean }>;
  export default plugin;
}
