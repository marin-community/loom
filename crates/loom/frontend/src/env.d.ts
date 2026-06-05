declare module '*.vue' {
  import type { DefineComponent } from 'vue';
  const component: DefineComponent<Record<string, unknown>, Record<string, unknown>, unknown>;
  export default component;
}

// markdown-it-task-lists ships no type declarations; it's a standard
// markdown-it plugin (a `PluginWithOptions`-shaped function).
declare module 'markdown-it-task-lists' {
  import type { PluginWithOptions } from 'markdown-it';
  const plugin: PluginWithOptions<{ enabled?: boolean; label?: boolean; labelAfter?: boolean }>;
  export default plugin;
}
