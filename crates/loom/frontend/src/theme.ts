import { ref } from 'vue';

export type Theme = 'light' | 'dark';

const STORAGE_KEY = 'loom-theme';

function preferred(): Theme {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === 'light' || stored === 'dark') return stored;
  // Loom is an operator console: fresh installs lead with the terminal palette.
  // The explicit light choice remains sticky once selected.
  return 'dark';
}

function apply(t: Theme) {
  document.documentElement.classList.toggle('dark', t === 'dark');
}

export const theme = ref<Theme>(preferred());

// Apply once at module load so there's no flash of the wrong palette.
apply(theme.value);

export function setTheme(t: Theme) {
  theme.value = t;
  localStorage.setItem(STORAGE_KEY, t);
  apply(t);
}

export function toggleTheme() {
  setTheme(theme.value === 'dark' ? 'light' : 'dark');
}
