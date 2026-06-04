// Lazy-loaded Monaco — the editor that powers VS Code, shipped as a package.
//
// The full `monaco-editor` bundle is heavy (multiple MB), so it is pulled in via
// dynamic import the first time the file viewer needs it, keeping it out of the
// main app chunk. Everything here is framework-agnostic; the Vue component drives
// it.

import type * as Monaco from 'monaco-editor';

let monacoPromise: Promise<typeof Monaco> | null = null;
let configured = false;

/** Load (once) and return the Monaco module, with workers wired up for rspack. */
export function loadMonaco(): Promise<typeof Monaco> {
  if (monacoPromise) return monacoPromise;

  // rspack / webpack-5 worker wiring: `new Worker(new URL(...))` makes the
  // bundler emit each worker as its own lazily-fetched chunk. Monaco asks for a
  // worker by language label; anything without a dedicated language service uses
  // the base editor worker. If a worker ever fails to load, Monaco degrades
  // gracefully — syntax highlighting (run on the main thread) still works.
  (self as unknown as { MonacoEnvironment: Monaco.Environment }).MonacoEnvironment = {
    getWorker(_workerId: string, label: string): Worker {
      switch (label) {
        case 'json':
          return new Worker(new URL('monaco-editor/esm/vs/language/json/json.worker', import.meta.url));
        case 'css':
        case 'scss':
        case 'less':
          return new Worker(new URL('monaco-editor/esm/vs/language/css/css.worker', import.meta.url));
        case 'html':
        case 'handlebars':
        case 'razor':
          return new Worker(new URL('monaco-editor/esm/vs/language/html/html.worker', import.meta.url));
        case 'typescript':
        case 'javascript':
          return new Worker(new URL('monaco-editor/esm/vs/language/typescript/ts.worker', import.meta.url));
        default:
          return new Worker(new URL('monaco-editor/esm/vs/editor/editor.worker', import.meta.url));
      }
    },
  };

  monacoPromise = import('monaco-editor').then((monaco) => {
    configureForViewing(monaco);
    return monaco;
  });
  return monacoPromise;
}

/** Monaco's built-in theme id for the current app theme. */
export function monacoTheme(dark: boolean): string {
  return dark ? 'vs-dark' : 'vs';
}

/** Map a file path to a registered Monaco language id, or `plaintext`. Uses
 *  Monaco's own language registry (no hand-maintained extension table). */
export function languageForPath(monaco: typeof Monaco, path: string): string {
  const name = path.split('/').pop() ?? '';
  const dot = name.lastIndexOf('.');
  const ext = dot >= 0 ? name.slice(dot).toLowerCase() : '';
  for (const lang of monaco.languages.getLanguages()) {
    if (lang.filenames?.includes(name)) return lang.id;
    if (ext && lang.extensions?.some((e) => e.toLowerCase() === ext)) return lang.id;
  }
  return 'plaintext';
}

// This is a read-only viewer, so language-service diagnostics (TypeScript's
// "cannot find module", JSON schema errors, …) would just be misleading red
// squiggles on code lifted out of its build context. Turn them off; syntax
// highlighting is unaffected.
function configureForViewing(monaco: typeof Monaco) {
  if (configured) return;
  configured = true;
  const ts = monaco.languages.typescript;
  if (ts) {
    for (const d of [ts.typescriptDefaults, ts.javascriptDefaults]) {
      d.setDiagnosticsOptions({ noSemanticValidation: true, noSyntaxValidation: true });
    }
  }
  monaco.languages.json?.jsonDefaults?.setDiagnosticsOptions({ validate: false });
}
